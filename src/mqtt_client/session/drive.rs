use embassy_time::{Duration, Instant, with_deadline};
use embedded_io_async::Error as _;

use crate::de::PacketReader;
use crate::mqtt_client::outbound::{
    CONTROL_PACKET_LEN, ControlAction, OutboundStep, SendState, check_control_packet_size,
    serialize_control_packet, serialize_pubrel,
};
use crate::{Connection, Error, InboundPublish, debug, error, trace, warn};

use super::Io;
use super::state::ROUND_TRIP_TIMEOUT_MS;

#[derive(Copy, Clone)]
enum FlushedPacket {
    Control(ControlAction),
    Release(u16),
    Retained(u16),
}

struct WriteStep<'a> {
    packet: FlushedPacket,
    bytes: &'a [u8],
    written: usize,
    len: usize,
}

enum PreparedStep<'a> {
    Write(WriteStep<'a>),
    Flush(FlushedPacket),
    Done,
}

#[derive(Copy, Clone)]
enum Progress {
    Idle,
    Advanced,
    Inbound(usize),
}

pub(super) async fn fill_packet_reader<'buf, C: Io>(
    packet_reader: &mut PacketReader<'buf>,
    connection: &mut C,
) -> Result<(), Error<C::Error>> {
    while !packet_reader.packet_available() {
        let buffer = packet_reader.receive_buffer()?;
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
        trace!("Read {=usize} transport bytes", count);
    }

    Ok(())
}

impl<'buf, IO: Io> Connection<'_, 'buf, IO> {
    async fn drive_packet(&mut self) -> Result<Progress, Error<IO::Error>> {
        if !self.live {
            return Err(Error::Disconnected);
        }
        let mut advanced = false;
        loop {
            if self.session.packet_reader.packet_available() {
                match self.process_received_packet()? {
                    Some(packet_length) => return Ok(Progress::Inbound(packet_length)),
                    None => {
                        advanced = true;
                        continue;
                    }
                }
            }

            let now = Instant::now();
            advanced |= self.service(now).await?;

            if self.session.packet_reader.packet_available() {
                match self.process_received_packet()? {
                    Some(packet_length) => return Ok(Progress::Inbound(packet_length)),
                    None => {
                        advanced = true;
                        continue;
                    }
                }
            }

            if self.session.data.outbound.next_step().is_none() {
                return Ok(if advanced {
                    Progress::Advanced
                } else {
                    Progress::Idle
                });
            }
        }
    }

    /// Advance local session state until an inbound publish is ready or the session would need to
    /// wait for new transport input or a future deadline.
    ///
    /// This is a cooperative progress step:
    /// - it does not wait for future inbound reads
    /// - it does not wait for future session deadlines
    /// - it may still await transport write or flush progress already needed for the current step
    ///
    /// Returns `Ok(None)` when no inbound publish is currently ready and further progress would
    /// require waiting. Wall-clock bounds depend on the transport: a stalled write or flush may
    /// still keep this future pending until the transport errors or is cancelled. Cancel-safe if
    /// the underlying transport I/O futures are cancel-safe.
    pub async fn drive(&mut self) -> Result<Option<InboundPublish<'_>>, Error<IO::Error>> {
        Ok(match self.drive_packet().await? {
            Progress::Inbound(packet_length) => Some(self.decode_inbound_publish(packet_length)),
            Progress::Idle | Progress::Advanced => None,
        })
    }

    /// Wait until any session progress happens or the session disconnects.
    ///
    /// Returns:
    /// - `Ok(Some(msg))` when that progress produced an inbound publish
    /// - `Ok(None)` when progress happened only internally, such as ACK handling, replay, or
    ///   keepalive traffic
    ///
    /// As with [`drive`](Self::drive), wall-clock bounds depend on the transport's read, write,
    /// and flush behavior.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe. Ordinary no-data must
    /// be represented by the transport future staying pending; `TimedOut` and `Interrupted` are
    /// treated as transport failure and disconnect the session.
    pub async fn poll(&mut self) -> Result<Option<InboundPublish<'_>>, Error<IO::Error>> {
        match self.wait_for_progress().await? {
            Progress::Inbound(packet_length) => {
                Ok(Some(self.decode_inbound_publish(packet_length)))
            }
            Progress::Advanced => Ok(None),
            Progress::Idle => unreachable!("wait_for_progress only returns after session progress"),
        }
    }

    async fn wait_for_progress(&mut self) -> Result<Progress, Error<IO::Error>> {
        loop {
            match self.drive_packet().await? {
                Progress::Inbound(packet_length) => {
                    return Ok(Progress::Inbound(packet_length));
                }
                Progress::Advanced => return Ok(Progress::Advanced),
                Progress::Idle => {}
            }

            let deadline = self.session.runtime.next_deadline();
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

    /// Wait until the next inbound publish arrives.
    ///
    /// This is a convenience wrapper over [`poll`](Self::poll) that skips internal-only
    /// progress.
    pub async fn recv(&mut self) -> Result<InboundPublish<'_>, Error<IO::Error>> {
        loop {
            match self.wait_for_progress().await? {
                Progress::Inbound(packet_length) => {
                    return Ok(self.decode_inbound_publish(packet_length));
                }
                Progress::Advanced => {}
                Progress::Idle => {
                    unreachable!("wait_for_progress only returns after session progress")
                }
            }
        }
    }

    pub(super) async fn service(&mut self, now: Instant) -> Result<bool, Error<IO::Error>> {
        let runtime = &mut self.session.runtime;
        if runtime
            .ping_timeout
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
        {
            warn!(
                "Keepalive ping timed out; disconnecting session next_ping={=?} ping_timeout={=?}",
                runtime.next_ping, runtime.ping_timeout
            );
            self.handle_disconnect();
            return Err(Error::Disconnected);
        }
        self.service_outbound_once(now).await
    }

    fn should_queue_pingreq(&self, now: Instant) -> bool {
        self.session.runtime.ping_timeout.is_none()
            && self
                .session
                .runtime
                .next_ping
                .is_some_and(|deadline| now >= deadline)
            && !self.session.data.outbound.has_pending_pingreq()
    }

    fn maybe_queue_pingreq(&mut self, now: Instant) -> Result<(), Error<IO::Error>> {
        if self.should_queue_pingreq(now) {
            check_control_packet_size(
                self.session.runtime.maximum_packet_size,
                ControlAction::PingReq,
            )?;
            self.session
                .data
                .outbound
                .queue_control(ControlAction::PingReq)?;
        }
        Ok(())
    }

    pub(super) async fn read_packet(&mut self) -> Result<(), Error<IO::Error>> {
        if !self.live {
            return Err(Error::Disconnected);
        }
        if let Err(err) = fill_packet_reader(&mut self.session.packet_reader, &mut self.io).await {
            match &err {
                Error::Transport(err) => warn!("Transport read failed: {}", err.kind()),
                Error::Disconnected => warn!("Transport returned EOF; disconnecting session"),
                _ => {}
            }
            self.handle_disconnect();
            return Err(err);
        }
        Ok(())
    }

    async fn service_outbound_once(&mut self, now: Instant) -> Result<bool, Error<IO::Error>> {
        self.maybe_queue_pingreq(now)?;
        let Some(step) = self.session.data.outbound.next_step() else {
            return Ok(false);
        };
        self.perform_outbound_step(step, now).await
    }

    fn complete_flush(&mut self, packet: FlushedPacket, now: Instant) {
        let runtime = &mut self.session.runtime;
        let data = &mut self.session.data;
        if matches!(packet, FlushedPacket::Control(ControlAction::PingReq)) {
            runtime.ping_timeout = Some(now + Duration::from_millis(ROUND_TRIP_TIMEOUT_MS));
        }
        runtime.note_outbound_activity(now);
        let found = match packet {
            FlushedPacket::Control(action) => data.outbound.flush_control(action),
            FlushedPacket::Release(packet_id) => data.outbound.flush_release(packet_id),
            FlushedPacket::Retained(packet_id) => data.outbound.flush_retained(packet_id),
        };
        debug_assert!(found, "completed outbound packet no longer tracked");
    }

    fn set_written(&mut self, packet: FlushedPacket, written: usize, len: usize) {
        let out = &mut self.session.data.outbound;
        let found = match packet {
            FlushedPacket::Control(action) => out.set_control_written(action, written, len),
            FlushedPacket::Release(packet_id) => out.set_release_written(packet_id, written, len),
            FlushedPacket::Retained(packet_id) => out.set_retained_written(packet_id, written, len),
        };
        debug_assert!(found, "outbound packet no longer tracked");
    }

    async fn perform_outbound_step(
        &mut self,
        step: OutboundStep,
        now: Instant,
    ) -> Result<bool, Error<IO::Error>> {
        let mut small_buf = [0u8; CONTROL_PACKET_LEN];
        let runtime = &mut self.session.runtime;
        let data = &mut self.session.data;
        let prepared = match step {
            OutboundStep::Control(step) => match step.state {
                SendState::Write { written } => {
                    trace!(
                        "Driving control packet {} progress_from={=usize} control={=usize} retained={=usize} pending_release={=usize}",
                        step.action,
                        written,
                        data.outbound.pending_control_len(),
                        data.outbound.retained_len(),
                        data.outbound.pending_release_len()
                    );
                    let packet = serialize_control_packet(
                        &mut small_buf,
                        step.action,
                        runtime.maximum_packet_size,
                    )?;
                    PreparedStep::Write(WriteStep {
                        packet: FlushedPacket::Control(step.action),
                        bytes: packet,
                        written,
                        len: packet.len(),
                    })
                }
                SendState::Flush => {
                    trace!("Flushing control packet {}", step.action);
                    PreparedStep::Flush(FlushedPacket::Control(step.action))
                }
                SendState::Sent => PreparedStep::Done,
            },
            OutboundStep::Release(step) => match step.state {
                SendState::Write { written } => {
                    trace!(
                        "Driving PUBREL write packet_id={=u16} progress_from={=usize} control={=usize} retained={=usize} pending_release={=usize}",
                        step.packet_id,
                        written,
                        data.outbound.pending_control_len(),
                        data.outbound.retained_len(),
                        data.outbound.pending_release_len()
                    );
                    let packet = serialize_pubrel(
                        &mut small_buf,
                        step.packet_id,
                        step.reason,
                        runtime.maximum_packet_size,
                    )?;
                    PreparedStep::Write(WriteStep {
                        packet: FlushedPacket::Release(step.packet_id),
                        bytes: packet,
                        written,
                        len: packet.len(),
                    })
                }
                SendState::Flush => {
                    trace!("Flushing PUBREL packet packet_id={=u16}", step.packet_id);
                    PreparedStep::Flush(FlushedPacket::Release(step.packet_id))
                }
                SendState::Sent => PreparedStep::Done,
            },
            OutboundStep::Retained(step) => match step.state {
                SendState::Write { written } => {
                    debug!(
                        "Driving retained packet write packet_id={=u16} progress {=usize}/{=usize} control={=usize} tx_used={=usize} tx_capacity={=usize} retained={=usize} pending_release={=usize}",
                        step.packet_id,
                        written,
                        step.len,
                        data.outbound.pending_control_len(),
                        data.outbound.used(),
                        data.outbound.capacity(),
                        data.outbound.retained_len(),
                        data.outbound.pending_release_len()
                    );
                    runtime.require_packet_size(step.len)?;
                    PreparedStep::Write(WriteStep {
                        packet: FlushedPacket::Retained(step.packet_id),
                        bytes: data.outbound.retained_packet(step.offset, step.len),
                        written,
                        len: step.len,
                    })
                }
                SendState::Flush => {
                    debug!("Flushing retained packet packet_id={=u16}", step.packet_id);
                    PreparedStep::Flush(FlushedPacket::Retained(step.packet_id))
                }
                SendState::Sent => PreparedStep::Done,
            },
        };

        let packet = match prepared {
            PreparedStep::Write(packet) => packet,
            PreparedStep::Flush(packet) => {
                self.flush_current(packet, now).await?;
                return Ok(true);
            }
            PreparedStep::Done => return Ok(false),
        };

        if !self.live {
            return Err(Error::Disconnected);
        }
        let WriteStep {
            packet,
            bytes,
            written,
            len,
        } = packet;
        let count = match write_current(&mut self.io, &bytes[written..]).await {
            Ok(count) => count,
            Err(Error::Transport(err)) => {
                warn!("Outbound packet write failed: {}", err.kind());
                self.handle_disconnect();
                return Err(Error::Transport(err));
            }
            Err(err) => return Err(err),
        };
        let written = written + count;
        self.set_written(packet, written, len);
        if written < len {
            return Ok(true);
        }
        self.flush_current(packet, now).await?;
        Ok(true)
    }

    async fn flush_current(
        &mut self,
        packet: FlushedPacket,
        now: Instant,
    ) -> Result<(), Error<IO::Error>> {
        if !self.live {
            return Err(Error::Disconnected);
        }
        if let Err(err) = self.io.flush().await {
            warn!("Outbound packet flush failed: {}", err.kind());
            self.handle_disconnect();
            return Err(Error::Transport(err));
        }
        self.complete_flush(packet, now);
        Ok(())
    }

    pub(super) async fn flush_outbound(&mut self) -> Result<(), Error<IO::Error>> {
        loop {
            self.maybe_queue_pingreq(Instant::now())?;
            let Some(step) = self.session.data.outbound.next_step() else {
                return Ok(());
            };
            self.perform_outbound_step(step, Instant::now()).await?;
        }
    }
}

async fn write_current<C: Io>(connection: &mut C, bytes: &[u8]) -> Result<usize, Error<C::Error>> {
    match connection.write(bytes).await {
        Ok(0) => {
            error!("transport write returned zero bytes for non-empty buffer");
            Err(Error::WriteZero)
        }
        Ok(count) => Ok(count),
        Err(err) => Err(Error::Transport(err)),
    }
}
