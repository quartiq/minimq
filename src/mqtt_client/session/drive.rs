use embassy_time::{Duration, Instant, with_deadline};
use embedded_io_async::Error as _;

use crate::de::PacketReader;
use crate::{Error, InboundPublish, debug, error, trace, warn};

use super::super::outbound::{
    ControlAction, OutboundStep, SendState, check_control_packet_size, serialize_control_packet,
    serialize_pubrel,
};
use super::state::PING_TIMEOUT_MS;
use super::{Io, Session};

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
    async fn drive_packet(&mut self) -> Result<Option<usize>, Error<IO::Error>> {
        if self.connection.is_none() {
            return Err(Error::Disconnected);
        }

        loop {
            if self.packet_reader.packet_available() {
                match self.process_received_packet()? {
                    Some(packet_length) => return Ok(Some(packet_length)),
                    None => continue,
                }
            }

            let now = Instant::now();
            self.service(now).await?;

            if self.packet_reader.packet_available() {
                match self.process_received_packet()? {
                    Some(packet_length) => return Ok(Some(packet_length)),
                    None => continue,
                }
            }

            if self.data.outbound.next_step().is_none() {
                return Ok(None);
            }
        }
    }

    /// Advance local session state until an inbound publish is ready or the session would need to
    /// wait for new transport input or a future deadline.
    ///
    /// Returns `Ok(None)` when no inbound publish is currently ready and the caller would need to
    /// block for more progress. Cancel-safe if the underlying transport I/O futures are
    /// cancel-safe.
    pub async fn drive(&mut self) -> Result<Option<InboundPublish<'_>>, Error<IO::Error>> {
        Ok(self
            .drive_packet()
            .await?
            .map(|packet_length| self.decode_inbound_publish(packet_length)))
    }

    /// Wait until an inbound publish arrives or the session disconnects.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe. Ordinary no-data must
    /// be represented by the transport future staying pending; `TimedOut` and `Interrupted` are
    /// treated as transport failure and disconnect the session.
    pub async fn poll(&mut self) -> Result<InboundPublish<'_>, Error<IO::Error>> {
        loop {
            if let Some(packet_length) = self.drive_packet().await? {
                return Ok(self.decode_inbound_publish(packet_length));
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
        }
    }

    pub(super) async fn service(&mut self, now: Instant) -> Result<(), Error<IO::Error>> {
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
        self.service_outbound_once(now).await
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

    async fn service_outbound_once(&mut self, now: Instant) -> Result<(), Error<IO::Error>> {
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
        macro_rules! write_or_disconnect {
            ($res:expr, $write_failed:literal) => {
                match $res {
                    Ok(0) => {
                        error!("transport write returned zero bytes for non-empty buffer");
                        return Err(Error::WriteZero);
                    }
                    Ok(count) => count,
                    Err(err) => {
                        warn!($write_failed, err.kind());
                        self.handle_disconnect();
                        return Err(Error::Transport(err));
                    }
                }
            };
        }

        macro_rules! flush_or_disconnect {
            ($res:expr, $flush_failed:literal) => {
                if let Err(err) = $res {
                    warn!($flush_failed, err.kind());
                    self.handle_disconnect();
                    return Err(Error::Transport(err));
                }
            };
        }

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
                    let count = write_or_disconnect!(res, "Control packet write failed: {:?}");
                    self.data.outbound.set_control_written(
                        step.action,
                        written + count,
                        packet.len(),
                    );
                    if written + count < packet.len() {
                        return Ok(());
                    }
                    trace!("Flushing control packet {:?}", step.action);
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    };
                    flush_or_disconnect!(res, "Control packet flush failed: {:?}");
                    self.complete_flush(FlushedPacket::Control(step.action), now);
                }
                SendState::Flush => {
                    trace!("Flushing control packet {:?}", step.action);
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    };
                    flush_or_disconnect!(res, "Control packet flush failed: {:?}");
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
                    let count = write_or_disconnect!(res, "PUBREL write failed: {:?}");
                    self.data.outbound.set_release_written(
                        step.packet_id,
                        written + count,
                        packet.len(),
                    );
                    if written + count < packet.len() {
                        return Ok(());
                    }
                    trace!("Flushing PUBREL packet packet_id={}", step.packet_id);
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    };
                    flush_or_disconnect!(res, "PUBREL flush failed: {:?}");
                    self.complete_flush(FlushedPacket::Release(step.packet_id), now);
                }
                SendState::Flush => {
                    trace!("Flushing PUBREL packet packet_id={}", step.packet_id);
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    };
                    flush_or_disconnect!(res, "PUBREL flush failed: {:?}");
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
                    let count = write_or_disconnect!(res, "Retained packet write failed: {:?}");
                    self.data.outbound.set_retained_written(
                        step.packet_id,
                        written + count,
                        step.len,
                    );
                    if written + count < step.len {
                        return Ok(());
                    }
                    debug!("Flushing retained packet packet_id={}", step.packet_id);
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    };
                    flush_or_disconnect!(res, "Retained packet flush failed: {:?}");
                    self.complete_flush(FlushedPacket::Retained(step.packet_id), now);
                }
                SendState::Flush => {
                    debug!("Flushing retained packet packet_id={}", step.packet_id);
                    let res = {
                        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
                        connection.flush().await
                    };
                    flush_or_disconnect!(res, "Retained packet flush failed: {:?}");
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

    pub(super) async fn flush_outbound(&mut self) -> Result<(), Error<IO::Error>> {
        loop {
            self.maybe_queue_pingreq(Instant::now())?;
            let Some(step) = self.data.outbound.next_step() else {
                return Ok(());
            };
            self.perform_outbound_step(step, Instant::now()).await?;
        }
    }
}
