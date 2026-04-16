use crate::de::PacketReader;
use crate::packets::{Connect, DisconnectReq, PingReq, Pub, PubRel, Subscribe, Unsubscribe};
use crate::types::{Auth, Properties, TopicFilter, Utf8String};
use crate::will::WillSpec;
use crate::{Broker, Config, Error, Property, ProtocolError, PubError, QoS, debug, info};
use core::num::NonZeroU16;
use embassy_time::{Duration, Instant};
use embedded_io_async::{Error as _, ErrorKind};
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
    will: Option<WillSpec<'buf>>,
    auth: Option<Auth<'buf>>,
    session_expiry_interval: u32,
    downgrade_qos: bool,
}

impl<'buf> Core<'buf> {
    pub(super) fn new(config: Config<'buf>) -> Self {
        let Config {
            broker,
            buffers,
            will,
            client_id,
            keepalive_interval,
            session_expiry_interval,
            downgrade_qos,
            auth,
        } = config;

        Self {
            broker,
            client_id,
            packet_reader: PacketReader::new(buffers.rx),
            session: SessionData::new(buffers.tx),
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
            return !self.session.outbound.scratch_space().is_empty();
        }
        self.runtime.send_quota != 0 && self.session.outbound.can_retain()
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.runtime.next_deadline()
    }

    pub(super) async fn connect<C: Io>(&mut self, connection: &mut C) -> Result<(), Error> {
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

        write_packet(
            self.session.outbound.scratch_space(),
            connection,
            &Connect {
                keepalive,
                properties: Properties::Slice(&properties),
                client_id: Utf8String(client_id.as_str()),
                auth,
                will: will.as_ref().map(WillSpec::as_will),
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
    ) -> Result<(), Error> {
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
        self.drive_outbound(connection).await
    }

    pub(super) async fn unsubscribe<C: Io>(
        &mut self,
        connection: &mut C,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<(), Error> {
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
        self.drive_outbound(connection).await
    }

    pub(super) async fn publish<C: Io, P>(
        &mut self,
        connection: &mut C,
        publish: crate::publication::Publication<'_, P>,
    ) -> Result<(), PubError<P::Error>>
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
            self.drive_outbound(connection)
                .await
                .map_err(PubError::Session)?;
            return Ok(());
        }

        let maximum_packet_size = self.runtime.maximum_packet_size;
        let packet = self.serialize_publish(publish)?;
        Self::require_packet_size(maximum_packet_size, packet.len()).map_err(PubError::Session)?;
        if let Err(err) = connection.write_all(packet).await {
            self.handle_disconnect();
            return Err(PubError::Session(Error::Transport(err.kind())));
        }
        if let Err(err) = connection.flush().await {
            self.handle_disconnect();
            return Err(PubError::Session(Error::Transport(err.kind())));
        }

        Ok(())
    }

    pub(super) async fn maintain<C: Io>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<(), Error> {
        if self.runtime.state != ConnectionState::Active {
            return Ok(());
        }

        if self
            .runtime
            .ping_timeout
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
        {
            self.handle_disconnect();
            return Ok(());
        }
        self.drive_outbound(connection).await?;

        if self
            .runtime
            .next_ping
            .is_some_and(|deadline| now >= deadline)
        {
            if let Err(err) =
                write_control_packet(connection, &PingReq {}, self.runtime.maximum_packet_size)
                    .await
            {
                self.handle_disconnect();
                return Err(err);
            }
            self.runtime.ping_timeout = Some(now + Duration::from_millis(PING_TIMEOUT_MS));
            self.runtime.next_ping = Some(now + self.runtime.keepalive_interval / 2);
        }

        Ok(())
    }

    pub(super) async fn disconnect<C: Io>(&mut self, connection: &mut C) -> Result<(), Error> {
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
    ) -> Result<Option<InboundPublish<'_>>, Error> {
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

            let count = match connection.read(buffer).await {
                Ok(count) => count,
                Err(err) => {
                    let kind = err.kind();
                    if matches!(kind, ErrorKind::TimedOut | ErrorKind::Interrupted) {
                        return Err(Error::Transport(kind));
                    }
                    self.handle_disconnect();
                    return Err(Error::Transport(kind));
                }
            };
            if count == 0 {
                self.handle_disconnect();
                return Ok(None);
            }
            self.packet_reader.commit(count);
            debug!("Received {} bytes", count);
        }

        let packet = {
            let packet_reader = &mut self.packet_reader;
            packet_reader.received_packet()
        };
        let packet = match packet {
            Ok(packet) => packet,
            Err(err) => {
                self.session.outbound.arm_replay();
                self.runtime.disconnect();
                return Err(err.into());
            }
        };
        info!("Received {:?}", packet);
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
        self.session.outbound.arm_replay();
        self.runtime.disconnect();
        self.packet_reader.reset();
    }

    fn keepalive_secs(&self) -> u16 {
        self.runtime.keepalive_interval.as_secs() as u16
    }

    fn require_retained_slot(&self) -> Result<(), Error> {
        if self.session.outbound.retained_full() {
            return Err(ProtocolError::InflightMetadataExhausted.into());
        }
        Ok(())
    }

    fn require_packet_size(maximum_packet_size: Option<u32>, len: usize) -> Result<(), Error> {
        if maximum_packet_size.is_some_and(|max| len > max as usize) {
            return Err(ProtocolError::Failed(crate::ReasonCode::PacketTooLarge).into());
        }
        Ok(())
    }

    async fn drive_outbound<C: Io>(&mut self, connection: &mut C) -> Result<(), Error> {
        let mut release_buf = [0u8; 9];

        loop {
            if let Some(step) = self.session.outbound.next_retained_step() {
                match step.state {
                    SendState::Write { written } => {
                        Self::require_packet_size(self.runtime.maximum_packet_size, step.len)?;
                        let packet = self.session.outbound.retained_packet(step.offset, step.len);
                        let count = match connection.write(&packet[written..]).await {
                            Ok(0) => {
                                self.handle_disconnect();
                                return Err(Error::Transport(ErrorKind::WriteZero));
                            }
                            Ok(count) => count,
                            Err(err) => {
                                self.handle_disconnect();
                                return Err(Error::Transport(err.kind()));
                            }
                        };
                        self.session.outbound.set_retained_written(
                            step.packet_id,
                            written + count,
                            step.len,
                        );
                    }
                    SendState::Flush => {
                        if let Err(err) = connection.flush().await {
                            self.handle_disconnect();
                            return Err(Error::Transport(err.kind()));
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
                                self.handle_disconnect();
                                return Err(Error::Transport(ErrorKind::WriteZero));
                            }
                            Ok(count) => count,
                            Err(err) => {
                                self.handle_disconnect();
                                return Err(Error::Transport(err.kind()));
                            }
                        };
                        self.session.outbound.set_release_written(
                            step.packet_id,
                            written + count,
                            packet.len(),
                        );
                    }
                    SendState::Flush => {
                        if let Err(err) = connection.flush().await {
                            self.handle_disconnect();
                            return Err(Error::Transport(err.kind()));
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

    fn serialize_publish<P: crate::publication::ToPayload>(
        &mut self,
        packet: Pub<'_, P>,
    ) -> Result<&[u8], PubError<P::Error>> {
        crate::ser::MqttSerializer::pub_to_buffer(self.session.outbound.scratch_space(), packet)
            .map_err(|err| match err {
                crate::ser::PubError::Encode(err) => PubError::Session(Error::Protocol(err.into())),
                crate::ser::PubError::Payload(err) => PubError::Payload(err),
            })
    }
}
