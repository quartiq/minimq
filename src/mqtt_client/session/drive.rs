use super::state::PING_TIMEOUT_MS;
use super::*;
use embedded_io_async::Error as _;

#[derive(Copy, Clone)]
enum FlushedPacket {
    Control(ControlAction),
    Release(u16),
    Retained(u16),
}

pub(super) async fn fill_packet_reader<'buf, C: Io>(
    packet_reader: &mut PacketReader<'buf>,
    connection: &mut C,
) -> Result<(), Error<C::Error>> {
    while !packet_reader.packet_available() {
        let buffer = packet_reader.receive_buffer().map_err(Error::Protocol)?;
        if buffer.is_empty() {
            break;
        }

        let count = match connection.read(buffer).await {
            Ok(count) => count,
            Err(err) => return Err(Error::Transport(err)),
        };
        if count == 0 {
            return Err(Error::Disconnected);
        }
        packet_reader.commit(count);
        trace!("Read {} transport bytes", count);
    }

    Ok(())
}

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    /// Wait until an inbound publish arrives or the session disconnects.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe. Ordinary no-data must
    /// be represented by the transport future staying pending; `TimedOut` and `Interrupted` are
    /// treated as transport failure and disconnect the session.
    pub async fn poll(&mut self) -> Result<Event<'_>, Error<IO::Error>> {
        if self.connection.is_none() {
            return Err(Error::Disconnected);
        }

        loop {
            let now = Instant::now();
            self.maintain(now).await?;

            if self.packet_reader.packet_available() {
                match self.handle_received_packet()? {
                    Some(packet_length) => return Ok(self.inbound_event(packet_length)),
                    None => continue,
                }
            }

            let deadline = self.runtime.next_deadline();
            let read = self.read_packet();
            match deadline {
                Some(deadline) => match with_deadline(deadline, read).await {
                    Ok(Ok(())) => {}
                    Ok(Err(err)) => return Err(err),
                    Err(_) => continue,
                },
                None => read.await?,
            }

            match self.handle_received_packet()? {
                Some(packet_length) => return Ok(self.inbound_event(packet_length)),
                None => continue,
            }
        }
    }

    pub(super) async fn maintain(&mut self, now: Instant) -> Result<(), Error<IO::Error>> {
        if self
            .runtime
            .ping_timeout
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
        {
            warn!(
                "Keepalive ping timed out; disconnecting session next_ping={:?} ping_timeout={:?}",
                self.runtime.next_ping, self.runtime.ping_timeout
            );
            self.handle_disconnect();
            return Err(Error::Disconnected);
        }
        self.drive_one_outbound(now).await
    }

    fn should_queue_pingreq(&self, now: Instant) -> bool {
        self.runtime.ping_timeout.is_none()
            && self
                .runtime
                .next_ping
                .is_some_and(|deadline| now >= deadline)
            && !self.data.outbound.has_pending_pingreq()
    }

    fn maybe_queue_pingreq(&mut self, now: Instant) -> Result<(), Error<IO::Error>> {
        if self.should_queue_pingreq(now) {
            check_control_packet_size(self.runtime.maximum_packet_size, ControlAction::PingReq)
                .map_err(Error::Protocol)?;
            self.data
                .outbound
                .queue_control(ControlAction::PingReq)
                .map_err(Error::Protocol)?;
        }
        Ok(())
    }

    pub(super) async fn read_packet(&mut self) -> Result<(), Error<IO::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Err(Error::Disconnected);
        };
        if let Err(err) = fill_packet_reader(&mut self.packet_reader, connection).await {
            match &err {
                Error::Transport(err) => warn!("Transport read failed: {:?}", err.kind()),
                Error::Disconnected => warn!("Transport returned EOF; disconnecting session"),
                _ => {}
            }
            self.handle_disconnect();
            return Err(err);
        }
        Ok(())
    }

    async fn drive_one_outbound(&mut self, now: Instant) -> Result<(), Error<IO::Error>> {
        self.maybe_queue_pingreq(now)?;
        let Some(step) = self.data.outbound.next_step() else {
            return Ok(());
        };
        self.perform_outbound_step(step, now).await
    }

    fn complete_flush(&mut self, packet: FlushedPacket, now: Instant) {
        if matches!(packet, FlushedPacket::Control(ControlAction::PingReq)) {
            self.runtime.ping_timeout = Some(now + Duration::from_millis(PING_TIMEOUT_MS));
        }
        self.runtime.note_outbound_activity(now);
        match packet {
            FlushedPacket::Control(action) => self.data.outbound.flush_control(action),
            FlushedPacket::Release(packet_id) => self.data.outbound.flush_release(packet_id),
            FlushedPacket::Retained(packet_id) => self.data.outbound.flush_retained(packet_id),
        }
    }

    async fn perform_outbound_step(
        &mut self,
        step: OutboundStep,
        now: Instant,
    ) -> Result<(), Error<IO::Error>> {
        let mut small_buf = [0u8; 9];
        match step {
            OutboundStep::Control(step) => match step.state {
                SendState::Write { written } => {
                    trace!(
                        "Driving control packet {:?} progress_from={} control={} retained={} pending_release={}",
                        step.action,
                        written,
                        self.data.outbound.pending_control_len(),
                        self.data.outbound.retained_len(),
                        self.data.outbound.pending_release_len()
                    );
                    let packet = serialize_control_packet(
                        &mut small_buf,
                        step.action,
                        self.runtime.maximum_packet_size,
                    )?;
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.write(&packet[written..]).await
                    };
                    let count = match res {
                        Ok(0) => {
                            warn!("Control packet write returned WriteZero");
                            self.handle_disconnect();
                            return Err(Error::WriteZero);
                        }
                        Ok(count) => count,
                        Err(err) => {
                            warn!("Control packet write failed: {:?}", err.kind());
                            self.handle_disconnect();
                            return Err(Error::Transport(err));
                        }
                    };
                    self.data.outbound.set_control_written(
                        step.action,
                        written + count,
                        packet.len(),
                    );
                    if written + count < packet.len() {
                        return Ok(());
                    }
                    trace!("Flushing control packet {:?}", step.action);
                    if let Err(err) = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    } {
                        warn!("Control packet flush failed: {:?}", err.kind());
                        self.handle_disconnect();
                        return Err(Error::Transport(err));
                    }
                    self.complete_flush(FlushedPacket::Control(step.action), now);
                }
                SendState::Flush => {
                    trace!("Flushing control packet {:?}", step.action);
                    if let Err(err) = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    } {
                        warn!("Control packet flush failed: {:?}", err.kind());
                        self.handle_disconnect();
                        return Err(Error::Transport(err));
                    }
                    self.complete_flush(FlushedPacket::Control(step.action), now);
                }
                SendState::Sent => {}
            },
            OutboundStep::Release(step) => match step.state {
                SendState::Write { written } => {
                    trace!(
                        "Driving PUBREL write packet_id={} progress_from={} control={} retained={} pending_release={}",
                        step.packet_id,
                        written,
                        self.data.outbound.pending_control_len(),
                        self.data.outbound.retained_len(),
                        self.data.outbound.pending_release_len()
                    );
                    let packet = serialize_pubrel(
                        &mut small_buf,
                        step.packet_id,
                        step.reason,
                        self.runtime.maximum_packet_size,
                    )?;
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.write(&packet[written..]).await
                    };
                    let count = match res {
                        Ok(0) => {
                            warn!("PUBREL write returned WriteZero");
                            self.handle_disconnect();
                            return Err(Error::WriteZero);
                        }
                        Ok(count) => count,
                        Err(err) => {
                            warn!("PUBREL write failed: {:?}", err.kind());
                            self.handle_disconnect();
                            return Err(Error::Transport(err));
                        }
                    };
                    self.data.outbound.set_release_written(
                        step.packet_id,
                        written + count,
                        packet.len(),
                    );
                    if written + count < packet.len() {
                        return Ok(());
                    }
                    trace!("Flushing PUBREL packet packet_id={}", step.packet_id);
                    if let Err(err) = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    } {
                        warn!("PUBREL flush failed: {:?}", err.kind());
                        self.handle_disconnect();
                        return Err(Error::Transport(err));
                    }
                    self.complete_flush(FlushedPacket::Release(step.packet_id), now);
                }
                SendState::Flush => {
                    trace!("Flushing PUBREL packet packet_id={}", step.packet_id);
                    if let Err(err) = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    } {
                        warn!("PUBREL flush failed: {:?}", err.kind());
                        self.handle_disconnect();
                        return Err(Error::Transport(err));
                    }
                    self.complete_flush(FlushedPacket::Release(step.packet_id), now);
                }
                SendState::Sent => {}
            },
            OutboundStep::Retained(step) => match step.state {
                SendState::Write { written } => {
                    debug!(
                        "Driving retained packet write packet_id={} progress {}/{} control={} tx_used={} tx_capacity={} retained={} pending_release={}",
                        step.packet_id,
                        written,
                        step.len,
                        self.data.outbound.pending_control_len(),
                        self.data.outbound.used(),
                        self.data.outbound.capacity(),
                        self.data.outbound.retained_len(),
                        self.data.outbound.pending_release_len()
                    );
                    self.runtime.require_packet_size(step.len)?;
                    let res = {
                        let packet = self.data.outbound.retained_packet(step.offset, step.len);
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.write(&packet[written..]).await
                    };
                    let count = match res {
                        Ok(0) => {
                            warn!("Retained packet write returned WriteZero");
                            self.handle_disconnect();
                            return Err(Error::WriteZero);
                        }
                        Ok(count) => count,
                        Err(err) => {
                            warn!("Retained packet write failed: {:?}", err.kind());
                            self.handle_disconnect();
                            return Err(Error::Transport(err));
                        }
                    };
                    self.data.outbound.set_retained_written(
                        step.packet_id,
                        written + count,
                        step.len,
                    );
                    if written + count < step.len {
                        return Ok(());
                    }
                    debug!("Flushing retained packet packet_id={}", step.packet_id);
                    if let Err(err) = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    } {
                        warn!("Retained packet flush failed: {:?}", err.kind());
                        self.handle_disconnect();
                        return Err(Error::Transport(err));
                    }
                    self.complete_flush(FlushedPacket::Retained(step.packet_id), now);
                }
                SendState::Flush => {
                    debug!("Flushing retained packet packet_id={}", step.packet_id);
                    if let Err(err) = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    } {
                        warn!("Retained packet flush failed: {:?}", err.kind());
                        self.handle_disconnect();
                        return Err(Error::Transport(err));
                    }
                    self.complete_flush(FlushedPacket::Retained(step.packet_id), now);
                }
                SendState::Sent => {}
            },
        }
        Ok(())
    }

    pub(super) fn handle_disconnect(&mut self) {
        debug!(
            "Resetting local session transport state and arming replay if needed control={} tx_used={} tx_capacity={} retained={} pending_release={}",
            self.data.outbound.pending_control_len(),
            self.data.outbound.used(),
            self.data.outbound.capacity(),
            self.data.outbound.retained_len(),
            self.data.outbound.pending_release_len()
        );
        self.connection = None;
        self.data.outbound.arm_replay();
        self.runtime.reset_transport();
        self.packet_reader.reset();
    }

    pub(super) async fn drive_outbound(&mut self) -> Result<(), Error<IO::Error>> {
        loop {
            self.maybe_queue_pingreq(Instant::now())?;
            let Some(step) = self.data.outbound.next_step() else {
                return Ok(());
            };
            self.perform_outbound_step(step, Instant::now()).await?;
        }
    }
}
