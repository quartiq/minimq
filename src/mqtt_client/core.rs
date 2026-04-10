use crate::de::PacketReader;
use crate::packets::{Connect, PingReq, Pub, PubRel, Subscribe};
use crate::types::{Auth, Properties, TopicFilter, Utf8String};
use crate::will::WillSpec;
use crate::{Broker, Config, Error, Property, ProtocolError, PubError, QoS, debug, info};
use core::mem;
use core::num::NonZeroU16;
use embassy_time::{Duration, Instant};
use embedded_io_async::{Error as _, ErrorKind, ErrorType, Read, Write};
use heapless::{String, Vec};

use super::InboundPublish;
use super::outbound::{
    MAX_PENDING_RELEASE, Outbound, PendingRelease, write_control_packet, write_packet,
};
use super::protocol::{PacketContext, handle_packet};

const PING_TIMEOUT_MS: u64 = 5_000;
const MAX_INBOUND_QOS2: usize = 4;
pub(super) const MAX_PENDING_SUBSCRIPTIONS: usize = 4;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) enum ConnectionState {
    Disconnected,
    Establishing,
    Active,
}

#[derive(Debug)]
pub(super) struct RuntimeState {
    pub(super) pending_subscriptions: Vec<u16, MAX_PENDING_SUBSCRIPTIONS>,
    pub(super) state: ConnectionState,
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
            pending_subscriptions: Vec::new(),
            state: ConnectionState::Disconnected,
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
        self.pending_subscriptions.clear();
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
    reset_pending: bool,
}

impl<'a> SessionData<'a> {
    fn new(outbound: &'a mut [u8]) -> Self {
        Self {
            packet_id: NonZeroU16::new(1).unwrap(),
            outbound: Outbound::new(outbound),
            pending_server_packet_ids: Vec::new(),
            session_present: false,
            reset_pending: false,
        }
    }

    pub(super) fn register_connected(&mut self) {
        self.session_present = true;
    }

    pub(super) fn reset(&mut self) {
        self.reset_pending = self.session_present;
        self.session_present = false;
        self.packet_id = NonZeroU16::new(1).unwrap();
        self.outbound.clear();
        self.pending_server_packet_ids.clear();
    }

    pub(super) fn take_reset(&mut self) -> bool {
        mem::take(&mut self.reset_pending)
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
            downgrade_qos,
            auth,
        } = config;

        Self {
            broker,
            client_id,
            packet_reader: PacketReader::new(buffers.rx),
            session: SessionData::new(buffers.outbound),
            runtime: RuntimeState::new(keepalive_interval),
            will,
            auth,
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

    pub(super) fn session_present(&self) -> bool {
        self.session.session_present
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

    pub(super) async fn connect<C>(&mut self, connection: &mut C) -> Result<(), Error>
    where
        C: Read + Write + ErrorType,
        C::Error: embedded_io_async::Error,
    {
        let client_id = self.client_id.clone();
        let properties = [
            Property::MaximumPacketSize(self.packet_reader.buffer.len() as u32),
            Property::SessionExpiryInterval(u32::MAX),
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

    pub(super) async fn subscribe<C>(
        &mut self,
        connection: &mut C,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error>
    where
        C: Read + Write + ErrorType,
        C::Error: embedded_io_async::Error,
    {
        if self.runtime.state != ConnectionState::Active {
            return Err(Error::Disconnected);
        }

        let packet_id = self.session.next_packet_id();
        self.write_packet(
            connection,
            &Subscribe {
                packet_id,
                properties: Properties::Slice(properties),
                topics,
            },
        )
        .await?;
        self.runtime
            .pending_subscriptions
            .push(packet_id)
            .map_err(|_| ProtocolError::InflightMetadataExhausted)?;
        Ok(())
    }

    pub(super) async fn publish<C, P>(
        &mut self,
        connection: &mut C,
        publish: crate::publication::Publication<'_, P>,
    ) -> Result<(), PubError<P::Error>>
    where
        C: Read + Write + ErrorType,
        C::Error: embedded_io_async::Error,
        P: crate::publication::ToPayload,
    {
        if self.runtime.state != ConnectionState::Active {
            return Err(PubError::Error(Error::Disconnected));
        }

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
        if packet_id.is_some() && self.session.outbound.retained_full() {
            return Err(PubError::Error(
                ProtocolError::InflightMetadataExhausted.into(),
            ));
        }

        if !self.can_publish(qos) {
            return Err(PubError::Error(Error::NotReady));
        }

        if let Some(packet_id) = packet_id {
            let (offset, len) = self.session.outbound.encode_publish(publish)?;
            let packet = self.session.outbound.retained_packet(offset, len);
            if let Err(err) = connection.write_all(packet).await {
                self.handle_disconnect();
                return Err(PubError::Error(Error::Transport(err.kind())));
            }
            if let Err(err) = connection.flush().await {
                self.handle_disconnect();
                return Err(PubError::Error(Error::Transport(err.kind())));
            }
            self.session
                .outbound
                .retain_publish(packet_id, offset, len)
                .map_err(|err| PubError::Error(err.into()))?;
            self.runtime.send_quota = self.runtime.send_quota.saturating_sub(1);
            return Ok(());
        }

        let packet = self.serialize_publish(publish)?;
        if let Err(err) = connection.write_all(packet).await {
            self.handle_disconnect();
            return Err(PubError::Error(Error::Transport(err.kind())));
        }
        if let Err(err) = connection.flush().await {
            self.handle_disconnect();
            return Err(PubError::Error(Error::Transport(err.kind())));
        }

        Ok(())
    }

    pub(super) async fn maintain<C>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<(), Error>
    where
        C: Read + Write + ErrorType,
        C::Error: embedded_io_async::Error,
    {
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

        if self.session.outbound.needs_replay() {
            self.session.outbound.mark_retained_dup();
            for packet in self.session.outbound.retained_packets() {
                connection
                    .write_all(packet)
                    .await
                    .map_err(|err| Error::Transport(err.kind()))?;
            }

            let pending_releases: Vec<PendingRelease, MAX_PENDING_RELEASE> =
                self.session.outbound.pending_releases().collect();
            for release in pending_releases {
                write_control_packet(
                    connection,
                    &PubRel {
                        packet_id: release.packet_id,
                        reason: release.reason.into(),
                    },
                )
                .await?;
            }

            self.session.outbound.finish_replay();
        }

        if self
            .runtime
            .next_ping
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
        {
            write_control_packet(connection, &PingReq {}).await?;
            self.runtime.ping_timeout = Some(now + Duration::from_millis(PING_TIMEOUT_MS));
            self.runtime.next_ping = Some(now + self.runtime.keepalive_interval / 2);
        }

        Ok(())
    }

    pub(super) async fn read<C>(
        &mut self,
        connection: &mut C,
        now: Instant,
    ) -> Result<Option<InboundPublish<'_>>, Error>
    where
        C: Read + Write + ErrorType,
        C::Error: embedded_io_async::Error,
    {
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
            packet_reader.received_packet()?
        };
        info!("Received {:?}", packet);
        let (result, transport_error) = {
            let mut ctx = PacketContext {
                client_id: &mut self.client_id,
                session: &mut self.session,
                runtime: &mut self.runtime,
            };
            let result = handle_packet(&mut ctx, connection, packet, now).await;
            let transport_error = matches!(result, Err(Error::Transport(_)));
            if transport_error {
                ctx.mark_disconnected();
            }
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

    pub(super) fn reset_reader(&mut self) {
        self.packet_reader.reset();
    }

    async fn write_packet<C, T>(&mut self, connection: &mut C, packet: &T) -> Result<(), Error>
    where
        C: Read + Write + ErrorType,
        C::Error: embedded_io_async::Error,
        T: serde::Serialize + crate::message_types::ControlPacket + core::fmt::Debug,
    {
        write_packet(self.session.outbound.scratch_space(), connection, packet).await
    }

    fn serialize_publish<P: crate::publication::ToPayload>(
        &mut self,
        packet: Pub<'_, P>,
    ) -> Result<&[u8], PubError<P::Error>> {
        crate::ser::MqttSerializer::pub_to_buffer(self.session.outbound.scratch_space(), packet)
            .map_err(|err| match err {
                crate::ser::PubError::Error(err) => PubError::Error(Error::Protocol(err.into())),
                crate::ser::PubError::Other(err) => PubError::Serialization(err),
            })
    }
}
