use crate::de::PacketReader;
use crate::de::received_packet::ReceivedPacket;
use crate::packets::{Connect, DisconnectReq, Pub, Subscribe, Unsubscribe};
use crate::ser::MAX_FIXED_HEADER_SIZE;
use crate::types::{Auth, Properties, TopicFilter, Utf8String};
use crate::{
    ConfigBuilder, Error, Property, ProtocolError, PubError, QoS, Will, debug, info, trace, warn,
};
use core::convert::TryFrom;
use core::num::NonZeroU16;
use embassy_time::{Duration, Instant};
use embedded_io_async::Error as _;
use embedded_io_async::ErrorKind;
use heapless::{String, Vec};

use super::Io;
use super::outbound::{
    ControlAction, Outbound, OutboundStep, SendState, check_control_packet_size,
    serialize_control_packet, serialize_pubrel, write_packet,
};
use super::protocol::{PacketContext, PacketOutcome, handle_packet};

const PING_TIMEOUT_MS: u64 = 5_000;
const MAX_INBOUND_QOS2: usize = 8;

macro_rules! write_step_or_disconnect {
    ($self:expr, $connection:expr, $bytes:expr, $context:literal) => {{
        match $connection.write($bytes).await {
            Ok(0) => {
                warn!(concat!($context, " write returned WriteZero"));
                $self.handle_disconnect();
                return Err(Error::WriteZero);
            }
            Ok(count) => count,
            Err(err) => {
                warn!(concat!($context, " write failed: {:?}"), err.kind());
                $self.handle_disconnect();
                return Err(Error::Transport(err));
            }
        }
    }};
}

macro_rules! flush_step_or_disconnect {
    ($self:expr, $connection:expr, $context:literal) => {{
        if let Err(err) = $connection.flush().await {
            warn!(concat!($context, " flush failed: {:?}"), err.kind());
            $self.handle_disconnect();
            return Err(Error::Transport(err));
        }
    }};
}

#[derive(Copy, Clone)]
enum ReadMode {
    Bounded,
    Blocking,
}

#[derive(Copy, Clone)]
enum FlushedPacket {
    Control(ControlAction),
    Release(u16),
    Retained(u16),
}

#[derive(Debug)]
pub(super) struct RuntimeState {
    pub(super) session_resumed: bool,
    pub(super) keepalive_interval: Duration,
    pub(super) send_quota: u16,
    pub(super) max_send_quota: u16,
    pub(super) maximum_packet_size: Option<u32>,
    pub(super) max_qos: Option<QoS>,
    pub(super) next_ping: Option<Instant>,
    pub(super) ping_timeout: Option<Instant>,
}

impl RuntimeState {
    fn new(keepalive_interval: Duration) -> Self {
        Self {
            session_resumed: false,
            keepalive_interval,
            send_quota: u16::MAX,
            max_send_quota: u16::MAX,
            maximum_packet_size: None,
            max_qos: None,
            next_ping: None,
            ping_timeout: None,
        }
    }

    pub(super) fn reset_transport(&mut self) {
        self.session_resumed = false;
        self.next_ping = None;
        self.ping_timeout = None;
    }

    pub(super) fn note_outbound_activity(&mut self, now: Instant) {
        self.next_ping = self
            .keepalive_send_interval()
            .map(|interval| now + interval);
    }

    fn keepalive_send_interval(&self) -> Option<Duration> {
        let keepalive_ms = self.keepalive_interval.as_millis();
        if keepalive_ms == 0 {
            return None;
        }

        let lead_ms = PING_TIMEOUT_MS.min(keepalive_ms / 2);
        Some(Duration::from_millis(keepalive_ms - lead_ms))
    }
}

#[derive(Debug)]
pub(super) struct SessionData<'a> {
    packet_id: NonZeroU16,
    pub(super) outbound: Outbound<'a>,
    pub(super) pending_server_packet_ids: Vec<u16, MAX_INBOUND_QOS2>,
    session_present: bool,
}

impl<'a> SessionData<'a> {
    fn new(outbound: &'a mut [u8]) -> Self {
        Self {
            packet_id: NonZeroU16::new(1).unwrap(),
            outbound: Outbound::new(outbound),
            pending_server_packet_ids: Vec::new(),
            session_present: false,
        }
    }

    pub(super) fn register_connected(&mut self) {
        self.session_present = true;
    }

    pub(super) fn reset(&mut self) {
        self.session_present = false;
        self.packet_id = NonZeroU16::new(1).unwrap();
        self.outbound.clear();
        self.pending_server_packet_ids.clear();
    }

    fn next_packet_id(&mut self) -> u16 {
        let packet_id = self.packet_id.get();
        self.packet_id =
            NonZeroU16::new(packet_id.wrapping_add(1)).unwrap_or(NonZeroU16::new(1).unwrap());
        packet_id
    }
}

#[derive(Debug)]
pub(super) struct Core<'buf> {
    client_id: String<64>,
    packet_reader: PacketReader<'buf>,
    session: SessionData<'buf>,
    runtime: RuntimeState,
    will: Option<Will<'buf>>,
    auth: Option<Auth<'buf>>,
    session_expiry_interval: u32,
    downgrade_qos: bool,
}

impl<'buf> Core<'buf> {
    pub(super) fn new(config: ConfigBuilder<'buf>) -> Self {
        let (
            buffers,
            will,
            client_id,
            keepalive_interval,
            session_expiry_interval,
            downgrade_qos,
            auth,
        ) = config.into_parts();
        let (rx, tx) = buffers.into_parts();

        Self {
            client_id,
            packet_reader: PacketReader::new(rx),
            session: SessionData::new(tx),
            runtime: RuntimeState::new(keepalive_interval),
            will,
            auth,
            session_expiry_interval,
            downgrade_qos,
        }
    }

    pub(super) fn rollback_transport(&mut self) {
        self.handle_disconnect();
    }

    pub(super) fn can_publish(&mut self, qos: QoS) -> bool {
        if qos == QoS::AtMostOnce {
            return self.session.outbound.scratch_space().len() >= MAX_FIXED_HEADER_SIZE;
        }
        self.runtime.send_quota != 0 && self.session.outbound.can_retain()
    }

    pub(super) fn is_publish_quiescent(&self) -> bool {
        self.session.outbound.is_quiescent()
    }

    pub(super) async fn connect<C: Io>(
        &mut self,
        connection: &mut C,
    ) -> Result<super::ConnectEvent, Error<C::Error>> {
        let client_id = self.client_id.clone();
        let properties = [
            Property::MaximumPacketSize(self.packet_reader.buffer.len() as u32),
            Property::SessionExpiryInterval(self.session_expiry_interval),
            Property::ReceiveMaximum(self.session.pending_server_packet_ids.capacity() as u16),
        ];
        let will = self.will.clone();
        let keepalive = self.runtime.keepalive_interval.as_secs() as u16;
        let clean_start = !self.session.session_present;
        let auth = self.auth;
        debug!(
            "Sending CONNECT: client_id={} clean_start={} keepalive_s={} session_expiry={} receive_max={} rx_max_packet_size={}",
            client_id,
            clean_start,
            keepalive,
            self.session_expiry_interval,
            self.session.pending_server_packet_ids.capacity(),
            self.packet_reader.buffer.len()
        );

        {
            let buffer = self.session.outbound.scratch_space();
            write_packet(
                buffer,
                connection,
                &Connect {
                    keepalive,
                    properties: Properties::Slice(&properties),
                    client_id: Utf8String(client_id.as_str()),
                    auth,
                    will,
                    clean_start,
                },
            )
            .await?;
        }

        self.runtime.next_ping = None;
        self.runtime.ping_timeout = None;

        if !self
            .read_packet_mode(connection, ReadMode::Blocking)
            .await?
        {
            return Err(Error::Protocol(ProtocolError::UnexpectedPacket));
        }
        let packet = {
            let packet_reader = &mut self.packet_reader;
            match packet_reader.received_packet() {
                Ok(packet) => packet,
                Err(err) => {
                    warn!("Failed to decode inbound packet: {:?}", err);
                    self.handle_disconnect();
                    return Err(err.into());
                }
            }
        };
        let ack = match packet {
            ReceivedPacket::ConnAck(ack) => ack,
            ReceivedPacket::Disconnect(_) => {
                info!("Received broker DISCONNECT during CONNECT");
                self.handle_disconnect();
                return Err(Error::Disconnected);
            }
            _ => {
                self.handle_disconnect();
                return Err(Error::Protocol(ProtocolError::UnexpectedPacket));
            }
        };

        let Self {
            client_id,
            session,
            runtime,
            ..
        } = self;

        if let Err(err) = ack.reason_code.as_result() {
            warn!("Broker rejected CONNECT with reason {:?}", ack.reason_code);
            runtime.reset_transport();
            return Err(Error::Protocol(err));
        }

        let resumed = ack.session_present;
        if !resumed {
            debug!("Broker started a fresh session; resetting local session state");
            session.reset();
        }
        let local_quota = session.outbound.max_inflight();
        let mut send_quota = local_quota;
        let mut max_send_quota = local_quota;
        let mut max_qos = None;
        let mut maximum_packet_size = None;
        let mut keepalive_interval = runtime.keepalive_interval;
        let mut assigned_client_id = None;

        for property in ack.properties.into_iter() {
            match match property {
                Ok(property) => property,
                Err(err) => {
                    runtime.reset_transport();
                    return Err(Error::Protocol(err));
                }
            } {
                Property::MaximumPacketSize(size) => maximum_packet_size = Some(size),
                Property::AssignedClientIdentifier(id) => {
                    assigned_client_id = Some(match String::try_from(id.0) {
                        Ok(client_id) => client_id,
                        Err(_) => {
                            runtime.reset_transport();
                            return Err(Error::Protocol(ProtocolError::ProvidedClientIdTooLong));
                        }
                    });
                }
                Property::ServerKeepAlive(keepalive) => {
                    keepalive_interval = Duration::from_secs(keepalive as u64);
                }
                Property::ReceiveMaximum(max) => {
                    if max == 0 {
                        runtime.reset_transport();
                        return Err(Error::Protocol(ProtocolError::InvalidProperty));
                    }
                    send_quota = max.min(local_quota);
                    max_send_quota = max.min(local_quota);
                }
                Property::MaximumQoS(max) => {
                    max_qos = Some(match QoS::try_from(max) {
                        Ok(qos) => qos,
                        Err(_) => {
                            runtime.reset_transport();
                            return Err(Error::Protocol(ProtocolError::WrongQos));
                        }
                    });
                }
                _ => {}
            }
        }

        runtime.session_resumed = resumed;
        runtime.keepalive_interval = keepalive_interval;
        runtime.send_quota = send_quota;
        runtime.max_send_quota = max_send_quota;
        runtime.max_qos = max_qos;
        runtime.maximum_packet_size = maximum_packet_size;
        if let Some(assigned_client_id) = assigned_client_id {
            *client_id = assigned_client_id;
        }

        debug!(
            "Activated session state resumed={} send_quota={}/{} max_qos={:?} broker_max_packet_size={:?}",
            resumed,
            runtime.send_quota,
            runtime.max_send_quota,
            runtime.max_qos,
            runtime.maximum_packet_size
        );

        session.register_connected();
        runtime.note_outbound_activity(Instant::now());
        runtime.ping_timeout = None;
        if resumed {
            info!("Connected and resumed existing broker session");
            Ok(super::ConnectEvent::Reconnected)
        } else {
            info!("Connected with a fresh broker session");
            Ok(super::ConnectEvent::Connected)
        }
    }

    pub(super) async fn subscribe<C: Io>(
        &mut self,
        connection: &mut C,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error<C::Error>> {
        if topics.is_empty() {
            return Err(ProtocolError::NoTopic.into());
        }
        self.drive_outbound(connection).await?;

        self.require_retained_slot()?;
        let packet_id = self.session.next_packet_id();
        let (offset, len) = self
            .session
            .outbound
            .encode_packet(&Subscribe {
                packet_id,
                dup: false,
                properties: Properties::Slice(properties),
                topics,
            })
            .map_err(Error::Protocol)?;
        Self::require_packet_size(self.runtime.maximum_packet_size, len)?;
        self.session
            .outbound
            .retain_packet(packet_id, offset, len)
            .map_err(Error::Protocol)?;
        debug!(
            "Enqueued SUBSCRIBE packet_id={} len={} tx_used={}",
            packet_id,
            len,
            self.session.outbound.used()
        );
        self.drive_outbound(connection).await
    }

    pub(super) async fn unsubscribe<C: Io>(
        &mut self,
        connection: &mut C,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<(), Error<C::Error>> {
        if topics.is_empty() {
            return Err(ProtocolError::NoTopic.into());
        }
        self.drive_outbound(connection).await?;

        self.require_retained_slot()?;
        let packet_id = self.session.next_packet_id();
        let (offset, len) = self
            .session
            .outbound
            .encode_packet(&Unsubscribe {
                packet_id,
                dup: false,
                properties: Properties::Slice(properties),
                topics,
            })
            .map_err(Error::Protocol)?;
        Self::require_packet_size(self.runtime.maximum_packet_size, len)?;
        self.session
            .outbound
            .retain_packet(packet_id, offset, len)
            .map_err(Error::Protocol)?;
        debug!(
            "Enqueued UNSUBSCRIBE packet_id={} len={} tx_used={}",
            packet_id,
            len,
            self.session.outbound.used()
        );
        self.drive_outbound(connection).await
    }

    pub(super) async fn publish<C: Io, P>(
        &mut self,
        connection: &mut C,
        publish: crate::publication::Publication<'_, P>,
    ) -> Result<(), PubError<P::Error, C::Error>>
    where
        P: crate::publication::ToPayload,
    {
        self.drive_outbound(connection)
            .await
            .map_err(PubError::Session)?;

        let mut publish: Pub<'_, P> = publish.into();
        if let Some(max_qos) = self.runtime.max_qos
            && self.downgrade_qos
            && publish.qos > max_qos
        {
            publish.qos = max_qos;
        }

        publish.packet_id = (publish.qos > QoS::AtMostOnce).then(|| self.session.next_packet_id());
        publish.dup = false;

        let packet_id = publish.packet_id;
        let qos = publish.qos;
        if packet_id.is_some() {
            self.require_retained_slot().map_err(PubError::Session)?;
        }

        if !self.can_publish(qos) {
            return Err(PubError::Session(Error::NotReady));
        }

        if let Some(packet_id) = packet_id {
            let (offset, len) = self.session.outbound.encode_publish(publish)?;
            Self::require_packet_size(self.runtime.maximum_packet_size, len)
                .map_err(PubError::Session)?;
            self.session
                .outbound
                .retain_packet(packet_id, offset, len)
                .map_err(|err| PubError::Session(Error::Protocol(err)))?;
            self.runtime.send_quota = self.runtime.send_quota.saturating_sub(1);
            debug!(
                "Enqueued PUBLISH packet_id={} qos={:?} len={} send_quota={}/{} tx_used={}",
                packet_id,
                qos,
                len,
                self.runtime.send_quota,
                self.runtime.max_send_quota,
                self.session.outbound.used()
            );
            self.drive_outbound(connection)
                .await
                .map_err(PubError::Session)?;
            return Ok(());
        }

        let maximum_packet_size = self.runtime.maximum_packet_size;
        let packet = self.serialize_publish(publish)?;
        Self::require_packet_size(maximum_packet_size, packet.len()).map_err(PubError::Session)?;
        debug!("Sending QoS0 PUBLISH len={}", packet.len());
        if let Err(err) = crate::mqtt_client::outbound::write_all(connection, packet).await {
            warn!("QoS0 PUBLISH write failed");
            self.handle_disconnect();
            return Err(PubError::Session(err));
        }
        if let Err(err) = connection.flush().await {
            warn!("QoS0 PUBLISH flush failed: {:?}", err.kind());
            self.handle_disconnect();
            return Err(PubError::Session(Error::Transport(err)));
        }
        self.runtime.note_outbound_activity(Instant::now());

        Ok(())
    }

    pub(super) async fn maintain_step<C: Io>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<bool, Error<C::Error>> {
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
        self.drive_outbound_step(connection, now).await
    }

    pub(super) async fn disconnect<C: Io>(
        &mut self,
        connection: &mut C,
    ) -> Result<(), Error<C::Error>> {
        info!("Graceful disconnect requested");
        let mut buffer = [0u8; 9];
        let result = write_packet(&mut buffer, connection, &DisconnectReq).await;
        self.handle_disconnect();
        result
    }

    pub(super) async fn step_event<C: Io>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<super::Event<'_>, Error<C::Error>> {
        self.maintain_step(connection, now).await?;
        self.read_step_event(connection, now).await
    }

    async fn read_step_event<C: Io>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<super::Event<'_>, Error<C::Error>> {
        if !self.read_packet_mode(connection, ReadMode::Bounded).await? {
            return Ok(super::Event::Idle);
        }
        match self.dispatch_received_packet(now)? {
            PacketOutcome::None => Ok(super::Event::Idle),
            PacketOutcome::Inbound(inbound) => Ok(super::Event::Inbound(inbound)),
        }
    }

    fn should_queue_pingreq(&self, now: Instant) -> bool {
        self.runtime.ping_timeout.is_none()
            && self
                .runtime
                .next_ping
                .is_some_and(|deadline| now >= deadline)
            && !self.session.outbound.has_pending_pingreq()
    }

    fn maybe_queue_pingreq<E>(&mut self, now: Instant) -> Result<(), Error<E>> {
        if self.should_queue_pingreq(now) {
            check_control_packet_size(self.runtime.maximum_packet_size, ControlAction::PingReq)
                .map_err(Error::Protocol)?;
            self.session
                .outbound
                .queue_control(ControlAction::PingReq)
                .map_err(Error::Protocol)?;
        }
        Ok(())
    }

    async fn read_packet_mode<C: Io>(
        &mut self,
        connection: &mut C,
        mode: ReadMode,
    ) -> Result<bool, Error<C::Error>> {
        while !self.packet_reader.packet_available() {
            let buffer = match self.packet_reader.receive_buffer() {
                Ok(buffer) => buffer,
                Err(err) => {
                    self.handle_disconnect();
                    return Err(err.into());
                }
            };
            if buffer.is_empty() {
                break;
            }
            if matches!(mode, ReadMode::Bounded)
                && !connection.read_ready().map_err(Error::Transport)?
            {
                return Ok(false);
            }

            let count = match connection.read(buffer).await {
                Ok(count) => count,
                Err(err) => {
                    let kind = err.kind();
                    if matches!(kind, ErrorKind::TimedOut | ErrorKind::Interrupted) {
                        trace!("Read interrupted/timed out while waiting for packet");
                        return match mode {
                            ReadMode::Bounded => Ok(false),
                            ReadMode::Blocking => {
                                self.handle_disconnect();
                                Err(Error::Transport(err))
                            }
                        };
                    }
                    warn!("Transport read failed: {:?}", kind);
                    self.handle_disconnect();
                    return Err(Error::Transport(err));
                }
            };
            if count == 0 {
                warn!("Transport returned EOF; disconnecting session");
                self.handle_disconnect();
                return Err(Error::Disconnected);
            }
            self.packet_reader.commit(count);
            trace!("Read {} transport bytes", count);
            if matches!(mode, ReadMode::Bounded) {
                let packet_ready = match self.packet_reader.receive_buffer() {
                    Ok(buffer) => buffer.is_empty(),
                    Err(err) => {
                        self.handle_disconnect();
                        return Err(err.into());
                    }
                };
                if !packet_ready {
                    return Ok(false);
                }
            }
        }

        Ok(self.packet_reader.packet_available())
    }

    fn dispatch_received_packet<E>(&mut self, now: Instant) -> Result<PacketOutcome<'_>, Error<E>> {
        if !self.packet_reader.packet_available() {
            return Ok(PacketOutcome::None);
        }

        let packet = {
            let packet_reader = &mut self.packet_reader;
            match packet_reader.received_packet() {
                Ok(packet) => packet,
                Err(err) => {
                    warn!("Failed to decode inbound packet: {:?}", err);
                    self.session.outbound.arm_replay();
                    self.runtime.reset_transport();
                    return Err(err.into());
                }
            }
        };
        let (result, disconnect) = {
            let mut ctx = PacketContext {
                session: &mut self.session,
                runtime: &mut self.runtime,
            };
            let result = handle_packet(&mut ctx, packet, now);
            let disconnect = Self::should_disconnect_after_packet(&result);
            (result, disconnect)
        };
        if disconnect {
            warn!("Disconnecting session after packet handling error");
            self.session.outbound.arm_replay();
            self.runtime.reset_transport();
        }
        result.map_err(Self::map_packet_error)
    }

    fn should_disconnect_after_packet(
        result: &Result<PacketOutcome<'_>, Error<core::convert::Infallible>>,
    ) -> bool {
        matches!(
            result,
            Err(Error::Protocol(
                ProtocolError::MalformedPacket
                    | ProtocolError::UnexpectedPacket
                    | ProtocolError::InvalidProperty
                    | ProtocolError::ProvidedClientIdTooLong
                    | ProtocolError::WrongQos
                    | ProtocolError::UnsupportedPacket
                    | ProtocolError::BadIdentifier
                    | ProtocolError::Deserialization(_)
            ))
        ) || matches!(
            result,
            Err(Error::Protocol(ProtocolError::Failed(
                crate::ReasonCode::PacketTooLarge
            )))
        )
    }

    fn map_packet_error<E>(err: Error<core::convert::Infallible>) -> Error<E> {
        match err {
            Error::Disconnected => Error::Disconnected,
            Error::Protocol(err) => Error::Protocol(err),
            Error::NotReady | Error::WriteZero => {
                unreachable!("packet handler returned local I/O state")
            }
            Error::Transport(never) => match never {},
        }
    }

    async fn drive_outbound_step<C: Io>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<bool, Error<C::Error>> {
        let Some(step) = self.next_outbound_step(now)? else {
            return Ok(false);
        };

        if !connection.write_ready().map_err(Error::Transport)? {
            return Ok(false);
        }

        self.perform_outbound_step(connection, step, now).await?;
        Ok(true)
    }

    fn next_outbound_step<E>(&mut self, now: Instant) -> Result<Option<OutboundStep>, Error<E>> {
        self.maybe_queue_pingreq(now)?;
        Ok(self.session.outbound.next_step())
    }

    fn complete_flush(&mut self, packet: FlushedPacket, now: Instant) {
        if matches!(packet, FlushedPacket::Control(ControlAction::PingReq)) {
            self.runtime.ping_timeout = Some(now + Duration::from_millis(PING_TIMEOUT_MS));
        }
        self.runtime.note_outbound_activity(now);
        match packet {
            FlushedPacket::Control(action) => self.session.outbound.flush_control(action),
            FlushedPacket::Release(packet_id) => self.session.outbound.flush_release(packet_id),
            FlushedPacket::Retained(packet_id) => self.session.outbound.flush_retained(packet_id),
        }
    }

    async fn perform_outbound_step<C: Io>(
        &mut self,
        connection: &mut C,
        step: OutboundStep,
        now: Instant,
    ) -> Result<(), Error<C::Error>> {
        let mut small_buf = [0u8; 9];
        match step {
            OutboundStep::Control(step) => match step.state {
                SendState::Write { written } => {
                    trace!(
                        "Driving control packet {:?} progress_from={} control={} retained={} pending_release={}",
                        step.action,
                        written,
                        self.session.outbound.pending_control_len(),
                        self.session.outbound.retained_len(),
                        self.session.outbound.pending_release_len()
                    );
                    let packet = serialize_control_packet(
                        &mut small_buf,
                        step.action,
                        self.runtime.maximum_packet_size,
                    )?;
                    let count = write_step_or_disconnect!(
                        self,
                        connection,
                        &packet[written..],
                        "Control packet"
                    );
                    self.session.outbound.set_control_written(
                        step.action,
                        written + count,
                        packet.len(),
                    );
                    if written + count < packet.len() {
                        return Ok(());
                    }
                    trace!("Flushing control packet {:?}", step.action);
                    flush_step_or_disconnect!(self, connection, "Control packet");
                    self.complete_flush(FlushedPacket::Control(step.action), now);
                }
                SendState::Flush => {
                    trace!("Flushing control packet {:?}", step.action);
                    flush_step_or_disconnect!(self, connection, "Control packet");
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
                        self.session.outbound.pending_control_len(),
                        self.session.outbound.retained_len(),
                        self.session.outbound.pending_release_len()
                    );
                    let packet = serialize_pubrel(
                        &mut small_buf,
                        step.packet_id,
                        step.reason,
                        self.runtime.maximum_packet_size,
                    )?;
                    let count =
                        write_step_or_disconnect!(self, connection, &packet[written..], "PUBREL");
                    self.session.outbound.set_release_written(
                        step.packet_id,
                        written + count,
                        packet.len(),
                    );
                    if written + count < packet.len() {
                        return Ok(());
                    }
                    trace!("Flushing PUBREL packet packet_id={}", step.packet_id);
                    flush_step_or_disconnect!(self, connection, "PUBREL");
                    self.complete_flush(FlushedPacket::Release(step.packet_id), now);
                }
                SendState::Flush => {
                    trace!("Flushing PUBREL packet packet_id={}", step.packet_id);
                    flush_step_or_disconnect!(self, connection, "PUBREL");
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
                        self.session.outbound.pending_control_len(),
                        self.session.outbound.used(),
                        self.session.outbound.capacity(),
                        self.session.outbound.retained_len(),
                        self.session.outbound.pending_release_len()
                    );
                    Self::require_packet_size(self.runtime.maximum_packet_size, step.len)?;
                    let count = {
                        let packet = self.session.outbound.retained_packet(step.offset, step.len);
                        write_step_or_disconnect!(
                            self,
                            connection,
                            &packet[written..],
                            "Retained packet"
                        )
                    };
                    self.session.outbound.set_retained_written(
                        step.packet_id,
                        written + count,
                        step.len,
                    );
                    if written + count < step.len {
                        return Ok(());
                    }
                    debug!("Flushing retained packet packet_id={}", step.packet_id);
                    flush_step_or_disconnect!(self, connection, "Retained packet");
                    self.complete_flush(FlushedPacket::Retained(step.packet_id), now);
                }
                SendState::Flush => {
                    debug!("Flushing retained packet packet_id={}", step.packet_id);
                    flush_step_or_disconnect!(self, connection, "Retained packet");
                    self.complete_flush(FlushedPacket::Retained(step.packet_id), now);
                }
                SendState::Sent => {}
            },
        }
        Ok(())
    }

    fn handle_disconnect(&mut self) {
        debug!(
            "Resetting local session transport state and arming replay if needed control={} tx_used={} tx_capacity={} retained={} pending_release={}",
            self.session.outbound.pending_control_len(),
            self.session.outbound.used(),
            self.session.outbound.capacity(),
            self.session.outbound.retained_len(),
            self.session.outbound.pending_release_len()
        );
        self.session.outbound.arm_replay();
        self.runtime.reset_transport();
        self.packet_reader.reset();
    }

    fn require_retained_slot<E>(&self) -> Result<(), Error<E>> {
        if self.session.outbound.retained_full() {
            return Err(ProtocolError::InflightMetadataExhausted.into());
        }
        Ok(())
    }

    fn require_packet_size<E>(
        maximum_packet_size: Option<u32>,
        len: usize,
    ) -> Result<(), Error<E>> {
        if maximum_packet_size.is_some_and(|max| len > max as usize) {
            return Err(ProtocolError::Failed(crate::ReasonCode::PacketTooLarge).into());
        }
        Ok(())
    }

    async fn drive_outbound<C: Io>(&mut self, connection: &mut C) -> Result<(), Error<C::Error>> {
        loop {
            let Some(step) = self.next_outbound_step(Instant::now())? else {
                return Ok(());
            };
            self.perform_outbound_step(connection, step, Instant::now())
                .await?;
        }
    }

    fn serialize_publish<P: crate::publication::ToPayload, E>(
        &mut self,
        packet: Pub<'_, P>,
    ) -> Result<&[u8], PubError<P::Error, E>> {
        crate::ser::MqttSerializer::pub_to_buffer(self.session.outbound.scratch_space(), packet)
            .map_err(|err| match err {
                crate::ser::PubError::Encode(err) => PubError::Session(Error::Protocol(err.into())),
                crate::ser::PubError::Payload(err) => PubError::Payload(err),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Buffers, ConfigBuilder, tests::block_on};
    use embedded_io_async::{ErrorType, Read, ReadReady, Write, WriteReady};
    use std::collections::VecDeque;
    use std::vec::Vec;

    #[derive(Default)]
    struct MockConnection {
        rx: VecDeque<Vec<u8>>,
        tx: Vec<Vec<u8>>,
        write_error: Option<ErrorKind>,
    }

    impl MockConnection {
        fn push_rx(&mut self, data: &[u8]) {
            self.rx.push_back(data.to_vec());
        }
    }

    impl ErrorType for MockConnection {
        type Error = ErrorKind;
    }

    impl Read for MockConnection {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let Some(mut chunk) = self.rx.pop_front() else {
                return Err(ErrorKind::TimedOut);
            };
            let len = buf.len().min(chunk.len());
            buf[..len].copy_from_slice(&chunk[..len]);
            if len < chunk.len() {
                chunk.drain(..len);
                self.rx.push_front(chunk);
            }
            Ok(len)
        }
    }

    impl ReadReady for MockConnection {
        fn read_ready(&mut self) -> Result<bool, Self::Error> {
            Ok(!self.rx.is_empty())
        }
    }

    impl Write for MockConnection {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            if let Some(err) = self.write_error.take() {
                return Err(err);
            }
            self.tx.push(buf.to_vec());
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    impl WriteReady for MockConnection {
        fn write_ready(&mut self) -> Result<bool, Self::Error> {
            Ok(true)
        }
    }

    fn core() -> Core<'static> {
        let rx = Box::leak(Box::new([0; 128]));
        let tx = Box::leak(Box::new([0; 1152]));
        Core::new(
            ConfigBuilder::new(Buffers::new(rx, tx))
                .client_id("test")
                .unwrap()
                .keepalive_interval(1),
        )
    }

    #[test]
    fn maintain_sends_pingreq_when_due() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        core.runtime.next_ping = Some(now);

        assert!(block_on(core.maintain_step(&mut connection, now)).unwrap());

        assert!(
            connection
                .tx
                .iter()
                .any(|frame| frame.as_slice() == [0xC0, 0x00])
        );
        assert_eq!(
            core.runtime.ping_timeout,
            Some(now + Duration::from_millis(PING_TIMEOUT_MS))
        );
        assert_eq!(
            core.runtime.next_ping,
            Some(now + core.runtime.keepalive_send_interval().unwrap())
        );
    }

    #[test]
    fn long_keepalive_schedules_ping_before_expiry() {
        let mut core = core();
        let now = Instant::now();
        core.runtime.keepalive_interval = Duration::from_secs(30);

        core.runtime.note_outbound_activity(now);

        assert_eq!(core.runtime.next_ping, Some(now + Duration::from_secs(25)));
    }

    #[test]
    fn maintain_does_not_send_second_pingreq_while_waiting_for_pingresp() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        core.runtime.next_ping = Some(now);

        assert!(block_on(core.maintain_step(&mut connection, now)).unwrap());
        assert!(
            !block_on(core.maintain_step(&mut connection, now + Duration::from_millis(600)))
                .unwrap()
        );

        let pingreqs = connection
            .tx
            .iter()
            .filter(|frame| frame.as_slice() == [0xC0, 0x00])
            .count();
        assert_eq!(pingreqs, 1);
    }

    #[test]
    fn pingresp_clears_keepalive_timeout() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        core.runtime.next_ping = Some(now);

        assert!(block_on(core.maintain_step(&mut connection, now)).unwrap());
        connection.push_rx(&[0xD0, 0x00]);

        for _ in 0..3 {
            let result = block_on(core.step_event(&mut connection, now));
            assert!(
                matches!(result, Ok(super::super::Event::Idle)),
                "{result:?}"
            );
            if core.runtime.ping_timeout.is_none() {
                break;
            }
        }
        assert_eq!(core.runtime.ping_timeout, None);
        assert!(core.runtime.next_ping.is_some());
    }

    #[test]
    fn inbound_publish_does_not_refresh_keepalive_deadline() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        core.runtime.next_ping = Some(deadline);
        connection.push_rx(&[0x30, 0x05, 0x00, 0x01, b'A', 0x00, 0x05]);

        for _ in 0..3 {
            let result = block_on(core.step_event(&mut connection, now)).unwrap();
            if matches!(result, super::super::Event::Inbound(_)) {
                break;
            }
            assert!(matches!(result, super::super::Event::Idle), "{result:?}");
        }
        assert_eq!(core.runtime.next_ping, Some(deadline));
    }

    #[test]
    fn qos0_publish_refreshes_keepalive_deadline() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        core.runtime.next_ping = Some(now);

        block_on(core.publish(&mut connection, crate::Publication::bytes("A", b"5"))).unwrap();
        assert!(core.runtime.next_ping.is_some_and(
            |deadline| deadline >= now + core.runtime.keepalive_send_interval().unwrap()
        ));
    }

    #[test]
    fn expired_ping_timeout_disconnects_session() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        core.runtime.ping_timeout = Some(now);

        let result = block_on(core.maintain_step(&mut connection, now));

        assert!(matches!(result, Err(Error::Disconnected)));
        assert_eq!(core.runtime.ping_timeout, None);
        assert_eq!(core.runtime.next_ping, None);
    }

    #[test]
    fn pingreq_write_error_disconnects_session() {
        let mut core = core();
        let mut connection = MockConnection {
            write_error: Some(ErrorKind::ConnectionReset),
            ..Default::default()
        };
        let now = Instant::now();
        core.runtime.next_ping = Some(now);

        let result = block_on(core.maintain_step(&mut connection, now));

        assert!(matches!(
            result,
            Err(Error::Transport(ErrorKind::ConnectionReset))
        ));
        assert_eq!(core.runtime.next_ping, None);
        assert_eq!(core.runtime.ping_timeout, None);
    }

    #[test]
    fn connect_uses_tx_buffer_when_rx_only_covers_connack() {
        let rx = Box::leak(Box::new([0; 8]));
        let tx = Box::leak(Box::new([0; 128]));
        let mut core = Core::new(
            ConfigBuilder::new(Buffers::new(rx, tx))
                .client_id("0123456789abcdef")
                .unwrap(),
        );
        let mut connection = MockConnection::default();
        connection.push_rx(&[0x20, 0x03, 0x00, 0x00, 0x00]);

        let result = block_on(core.connect(&mut connection));

        assert!(matches!(result, Ok(super::super::ConnectEvent::Connected)));
        assert_eq!(connection.tx.len(), 1);
        assert!(connection.tx[0].len() > rx.len());
    }

    #[test]
    fn connect_returns_insufficient_memory_when_tx_is_too_small() {
        let rx = Box::leak(Box::new([0; 8]));
        let tx = Box::leak(Box::new([0; MAX_FIXED_HEADER_SIZE - 1]));
        let mut core = Core::new(
            ConfigBuilder::new(Buffers::new(rx, tx))
                .client_id("test")
                .unwrap(),
        );
        let mut connection = MockConnection::default();

        let result = block_on(core.connect(&mut connection));

        assert!(matches!(
            result,
            Err(Error::Protocol(ProtocolError::Encode(
                crate::SerError::InsufficientMemory
            )))
        ));
    }
}
