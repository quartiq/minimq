use crate::de::PacketReader;
use crate::packets::{Connect, DisconnectReq, PingReq, Pub, PubRel, Subscribe, Unsubscribe};
use crate::ser::MAX_FIXED_HEADER_SIZE;
use crate::types::{Auth, Properties, TopicFilter, Utf8String};
use crate::{
    Broker, ConfigBuilder, Error, Property, ProtocolError, PubError, QoS, Will, debug, info, trace,
    warn,
};
use core::num::NonZeroU16;
use embassy_time::{Duration, Instant};
use embedded_io_async::Error as _;
use embedded_io_async::ErrorKind;
use heapless::{String, Vec};

use super::outbound::{Outbound, SendState, write_control_packet, write_packet};
use super::protocol::{PacketContext, handle_packet};
use super::{InboundPublish, Io};

const PING_TIMEOUT_MS: u64 = 5_000;
const MAX_INBOUND_QOS2: usize = 8;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) enum ConnectionState {
    Disconnected,
    Establishing,
    Active,
}

#[derive(Debug)]
pub(super) struct RuntimeState {
    pub(super) state: ConnectionState,
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
            state: ConnectionState::Disconnected,
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

    fn next_deadline(&self) -> Option<Instant> {
        match (self.next_ping, self.ping_timeout) {
            (Some(ping), Some(timeout)) => Some(ping.min(timeout)),
            (Some(ping), None) => Some(ping),
            (None, Some(timeout)) => Some(timeout),
            (None, None) => None,
        }
    }

    pub(super) fn disconnect(&mut self) {
        self.state = ConnectionState::Disconnected;
        self.session_resumed = false;
        self.next_ping = None;
        self.ping_timeout = None;
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
    broker: Broker<'buf>,
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
            broker,
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
            broker,
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

    pub(super) fn broker(&self) -> &Broker<'_> {
        &self.broker
    }

    pub(super) fn is_connected(&self) -> bool {
        self.runtime.state == ConnectionState::Active
    }

    pub(super) fn is_disconnected(&self) -> bool {
        self.runtime.state == ConnectionState::Disconnected
    }

    pub(super) fn session_resumed(&self) -> bool {
        self.runtime.session_resumed
    }

    pub(super) fn can_publish(&mut self, qos: QoS) -> bool {
        if self.runtime.state != ConnectionState::Active {
            return false;
        }
        if qos == QoS::AtMostOnce {
            return self.session.outbound.scratch_space().len() >= MAX_FIXED_HEADER_SIZE;
        }
        self.runtime.send_quota != 0 && self.session.outbound.can_retain()
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.runtime.next_deadline()
    }

    pub(super) async fn connect<C: Io>(
        &mut self,
        connection: &mut C,
    ) -> Result<(), Error<C::Error>> {
        let client_id = self.client_id.clone();
        let properties = [
            Property::MaximumPacketSize(self.packet_reader.buffer.len() as u32),
            Property::SessionExpiryInterval(self.session_expiry_interval),
            Property::ReceiveMaximum(self.session.pending_server_packet_ids.capacity() as u16),
        ];
        let will = self.will.clone();
        let keepalive = self.keepalive_secs();
        let clean_start = !self.session.session_present;
        let auth = self.auth;
        debug!(
            "Sending CONNECT: broker={:?} client_id={} clean_start={} keepalive_s={} session_expiry={} receive_max={} rx_max_packet_size={}",
            self.broker,
            client_id,
            clean_start,
            keepalive,
            self.session_expiry_interval,
            self.session.pending_server_packet_ids.capacity(),
            self.packet_reader.buffer.len()
        );

        write_packet(
            self.session.outbound.scratch_space(),
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

        self.runtime.state = ConnectionState::Establishing;
        self.runtime.next_ping = None;
        self.runtime.ping_timeout = None;
        Ok(())
    }

    pub(super) async fn subscribe<C: Io>(
        &mut self,
        connection: &mut C,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error<C::Error>> {
        if self.runtime.state != ConnectionState::Active {
            return Err(Error::Disconnected);
        }
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
        if self.runtime.state != ConnectionState::Active {
            return Err(Error::Disconnected);
        }
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
        if self.runtime.state != ConnectionState::Active {
            return Err(PubError::Session(Error::Disconnected));
        }
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

        Ok(())
    }

    pub(super) async fn maintain<C: Io>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<(), Error<C::Error>> {
        if self.runtime.state != ConnectionState::Active {
            return Ok(());
        }

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
            return Ok(());
        }
        self.drive_outbound(connection).await?;

        if self.runtime.ping_timeout.is_none()
            && self
                .runtime
                .next_ping
                .is_some_and(|deadline| now >= deadline)
        {
            trace!("Sending PINGREQ");
            if let Err(err) =
                write_control_packet(connection, &PingReq {}, self.runtime.maximum_packet_size)
                    .await
            {
                warn!("PINGREQ send failed");
                self.handle_disconnect();
                return Err(err);
            }
            self.runtime.ping_timeout = Some(now + Duration::from_millis(PING_TIMEOUT_MS));
            self.runtime.next_ping = Some(now + self.runtime.keepalive_interval / 2);
        }

        Ok(())
    }

    pub(super) async fn disconnect<C: Io>(
        &mut self,
        connection: &mut C,
    ) -> Result<(), Error<C::Error>> {
        info!("Graceful disconnect requested");
        let result = if self.runtime.state == ConnectionState::Active {
            write_control_packet(connection, &DisconnectReq, self.runtime.maximum_packet_size).await
        } else {
            Ok(())
        };
        self.handle_disconnect();
        result
    }

    pub(super) async fn read<C: Io>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<Option<InboundPublish<'_>>, Error<C::Error>> {
        if self.runtime.state == ConnectionState::Disconnected {
            self.packet_reader.reset();
        }

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

            let count = match connection.read(buffer).await {
                Ok(count) => count,
                Err(err) => {
                    let kind = err.kind();
                    if matches!(kind, ErrorKind::TimedOut | ErrorKind::Interrupted) {
                        trace!("Read interrupted/timed out while waiting for packet");
                        return Err(Error::Transport(err));
                    }
                    warn!("Transport read failed: {:?}", kind);
                    self.handle_disconnect();
                    return Err(Error::Transport(err));
                }
            };
            if count == 0 {
                warn!("Transport returned EOF; disconnecting session");
                self.handle_disconnect();
                return Ok(None);
            }
            self.packet_reader.commit(count);
            trace!("Read {} transport bytes", count);
        }

        let packet = {
            let packet_reader = &mut self.packet_reader;
            packet_reader.received_packet()
        };
        let packet = match packet {
            Ok(packet) => packet,
            Err(err) => {
                warn!("Failed to decode inbound packet: {:?}", err);
                self.session.outbound.arm_replay();
                self.runtime.disconnect();
                return Err(err.into());
            }
        };
        let (result, transport_error) = {
            let mut ctx = PacketContext {
                client_id: &mut self.client_id,
                session: &mut self.session,
                runtime: &mut self.runtime,
            };
            let result = handle_packet(&mut ctx, connection, packet, now).await;
            let disconnect = matches!(result, Err(Error::Transport(_)))
                || matches!(
                    result,
                    Err(Error::Protocol(
                        ProtocolError::MalformedPacket
                            | ProtocolError::UnexpectedPacket
                            | ProtocolError::InvalidProperty
                            | ProtocolError::WrongQos
                            | ProtocolError::UnsupportedPacket
                            | ProtocolError::BadIdentifier
                            | ProtocolError::Deserialization(_)
                    ))
                )
                || matches!(
                    result,
                    Err(Error::Protocol(ProtocolError::Failed(
                        crate::ReasonCode::PacketTooLarge
                    )))
                )
                || (ctx.runtime.state != ConnectionState::Active
                    && matches!(result, Err(Error::Protocol(ProtocolError::Failed(_)))));
            if disconnect {
                warn!("Disconnecting session after packet handling error");
                ctx.session.outbound.arm_replay();
                ctx.runtime.disconnect();
            }
            let transport_error = matches!(result, Err(Error::Transport(_)));
            (result, transport_error)
        };
        debug_assert!(!transport_error || self.runtime.state == ConnectionState::Disconnected);
        result
    }

    fn handle_disconnect(&mut self) {
        debug!(
            "Resetting local session transport state and arming replay if needed tx_used={} tx_capacity={} retained={} pending_release={}",
            self.session.outbound.used(),
            self.session.outbound.capacity(),
            self.session.outbound.retained_len(),
            self.session.outbound.pending_release_len()
        );
        self.session.outbound.arm_replay();
        self.runtime.disconnect();
        self.packet_reader.reset();
    }

    fn keepalive_secs(&self) -> u16 {
        self.runtime.keepalive_interval.as_secs() as u16
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
        let mut release_buf = [0u8; 9];

        loop {
            if let Some(step) = self.session.outbound.next_retained_step() {
                match step.state {
                    SendState::Write { written } => {
                        trace!(
                            "Driving retained packet write packet_id={} progress {}/{} tx_used={} tx_capacity={} retained={} pending_release={}",
                            step.packet_id,
                            written,
                            step.len,
                            self.session.outbound.used(),
                            self.session.outbound.capacity(),
                            self.session.outbound.retained_len(),
                            self.session.outbound.pending_release_len()
                        );
                        Self::require_packet_size(self.runtime.maximum_packet_size, step.len)?;
                        let packet = self.session.outbound.retained_packet(step.offset, step.len);
                        let count = match connection.write(&packet[written..]).await {
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
                        self.session.outbound.set_retained_written(
                            step.packet_id,
                            written + count,
                            step.len,
                        );
                    }
                    SendState::Flush => {
                        trace!("Flushing retained packet packet_id={}", step.packet_id);
                        if let Err(err) = connection.flush().await {
                            warn!("Retained packet flush failed: {:?}", err.kind());
                            self.handle_disconnect();
                            return Err(Error::Transport(err));
                        }
                        self.session.outbound.flush_retained(step.packet_id);
                    }
                    SendState::Sent => {}
                }
                continue;
            }

            if let Some(step) = self.session.outbound.next_release_step() {
                match step.state {
                    SendState::Write { written } => {
                        trace!(
                            "Driving PUBREL write packet_id={} progress_from={} tx_used={} tx_capacity={} retained={} pending_release={}",
                            step.packet_id,
                            written,
                            self.session.outbound.used(),
                            self.session.outbound.capacity(),
                            self.session.outbound.retained_len(),
                            self.session.outbound.pending_release_len()
                        );
                        let packet = crate::ser::MqttSerializer::to_buffer(
                            &mut release_buf,
                            &PubRel {
                                packet_id: step.packet_id,
                                reason: step.reason.into(),
                            },
                        )
                        .map_err(|err| Error::Protocol(err.into()))?;
                        Self::require_packet_size(self.runtime.maximum_packet_size, packet.len())?;
                        let count = match connection.write(&packet[written..]).await {
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
                        self.session.outbound.set_release_written(
                            step.packet_id,
                            written + count,
                            packet.len(),
                        );
                    }
                    SendState::Flush => {
                        trace!("Flushing PUBREL packet packet_id={}", step.packet_id);
                        if let Err(err) = connection.flush().await {
                            warn!("PUBREL flush failed: {:?}", err.kind());
                            self.handle_disconnect();
                            return Err(Error::Transport(err));
                        }
                        self.session.outbound.flush_release(step.packet_id);
                    }
                    SendState::Sent => {}
                }
                continue;
            }

            return Ok(());
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
    use embedded_io_async::{ErrorType, Read, Write};
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

    fn core() -> Core<'static> {
        let broker = "127.0.0.1:1883"
            .parse::<std::net::SocketAddr>()
            .unwrap()
            .into();
        let rx = Box::leak(Box::new([0; 128]));
        let tx = Box::leak(Box::new([0; 1152]));
        Core::new(
            ConfigBuilder::new(broker, Buffers::new(rx, tx))
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
        core.runtime.state = ConnectionState::Active;
        core.runtime.next_ping = Some(now);

        block_on(core.maintain(&mut connection, now)).unwrap();

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
    }

    #[test]
    fn maintain_does_not_send_second_pingreq_while_waiting_for_pingresp() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        core.runtime.state = ConnectionState::Active;
        core.runtime.next_ping = Some(now);

        block_on(core.maintain(&mut connection, now)).unwrap();
        block_on(core.maintain(&mut connection, now + Duration::from_millis(600))).unwrap();

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
        core.runtime.state = ConnectionState::Active;
        core.runtime.next_ping = Some(now);

        block_on(core.maintain(&mut connection, now)).unwrap();
        connection.push_rx(&[0xD0, 0x00]);

        let result = block_on(core.read(&mut connection, now));
        assert!(matches!(result, Ok(None)), "{result:?}");
        assert_eq!(core.runtime.ping_timeout, None);
        assert_eq!(core.runtime.state, ConnectionState::Active);
    }

    #[test]
    fn expired_ping_timeout_disconnects_session() {
        let mut core = core();
        let mut connection = MockConnection::default();
        let now = Instant::now();
        core.runtime.state = ConnectionState::Active;
        core.runtime.ping_timeout = Some(now);

        block_on(core.maintain(&mut connection, now)).unwrap();

        assert_eq!(core.runtime.state, ConnectionState::Disconnected);
        assert_eq!(core.runtime.ping_timeout, None);
    }

    #[test]
    fn pingreq_write_error_disconnects_session() {
        let mut core = core();
        let mut connection = MockConnection {
            write_error: Some(ErrorKind::ConnectionReset),
            ..Default::default()
        };
        let now = Instant::now();
        core.runtime.state = ConnectionState::Active;
        core.runtime.next_ping = Some(now);

        let result = block_on(core.maintain(&mut connection, now));

        assert!(matches!(
            result,
            Err(Error::Transport(ErrorKind::ConnectionReset))
        ));
        assert_eq!(core.runtime.state, ConnectionState::Disconnected);
    }
}
