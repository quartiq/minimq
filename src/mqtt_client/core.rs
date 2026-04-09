use crate::{
    Broker, Config, Error, Property, ProtocolError, PubError, QoS, ReasonCode,
    de::{PacketReader, received_packet::ReceivedPacket},
    debug, info,
    packets::{Connect, PingReq, Pub, PubAck, PubComp, PubRec, PubRel, Subscribe},
    types::{Auth, Properties, TopicFilter, Utf8String},
    will::Will,
};
use core::convert::TryFrom;
use embassy_time::{Duration, Instant};
use embedded_io_async::{Error as _, ErrorKind, ErrorType, Read, Write};
use heapless::{String, Vec};

use super::InboundPublish;

const PING_TIMEOUT_MS: u64 = 5_000;
const MAX_OUTBOUND: usize = 16;
const MAX_INBOUND_QOS2: usize = 16;
const MAX_PENDING_SUBSCRIPTIONS: usize = 32;
const MAX_PENDING_PUBREL: usize = 16;

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) enum State {
    Disconnected,
    Establishing,
    Active,
}

#[derive(Debug, Copy, Clone, PartialEq)]
struct PendingPubrel {
    packet_id: u16,
    reason: ReasonCode,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct OutboundPublish {
    packet_id: u16,
    qos: QoS,
    offset: usize,
    len: usize,
}

#[derive(Debug)]
struct Inflight<'a> {
    buf: &'a mut [u8],
    used: usize,
    entries: Vec<OutboundPublish, MAX_OUTBOUND>,
    pending_pubrel: Vec<PendingPubrel, MAX_PENDING_PUBREL>,
    replay_pending: bool,
}

impl<'a> Inflight<'a> {
    fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            used: 0,
            entries: Vec::new(),
            pending_pubrel: Vec::new(),
            replay_pending: false,
        }
    }

    fn clear(&mut self) {
        self.used = 0;
        self.entries.clear();
        self.pending_pubrel.clear();
        self.replay_pending = false;
    }

    fn pending_transactions(&self) -> bool {
        !self.entries.is_empty() || !self.pending_pubrel.is_empty()
    }

    fn metadata_full(&self) -> bool {
        self.entries.is_full()
    }

    fn max_send_quota(&self) -> u16 {
        MAX_OUTBOUND.min(MAX_PENDING_PUBREL) as u16
    }

    fn can_publish(&mut self, max_tx_size: usize) -> bool {
        self.compact();
        self.entries.len() < self.entries.capacity()
            && self.buf.len().saturating_sub(self.used) >= max_tx_size
    }

    fn push_publish(
        &mut self,
        packet_id: u16,
        qos: QoS,
        packet: &[u8],
    ) -> Result<(), ProtocolError> {
        if self.entries.is_full() {
            return Err(ProtocolError::InflightMetadataExhausted);
        }
        self.compact();
        if self.buf.len().saturating_sub(self.used) < packet.len() {
            return Err(ProtocolError::BufferSize);
        }

        let offset = self.used;
        self.buf[offset..offset + packet.len()].copy_from_slice(packet);
        self.used += packet.len();
        self.entries
            .push(OutboundPublish {
                packet_id,
                qos,
                offset,
                len: packet.len(),
            })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)?;
        Ok(())
    }

    fn remove_publish(&mut self, packet_id: u16) -> Result<OutboundPublish, ProtocolError> {
        let position = self
            .entries
            .iter()
            .position(|entry| entry.packet_id == packet_id)
            .ok_or(ProtocolError::BadIdentifier)?;
        let entry = self.entries.swap_remove(position);
        self.compact();
        Ok(entry)
    }

    fn queue_pubrel(&mut self, packet_id: u16, reason: ReasonCode) -> Result<(), ProtocolError> {
        self.pending_pubrel
            .push(PendingPubrel { packet_id, reason })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)
    }

    fn remove_pubrel(&mut self, packet_id: u16) -> Result<(), ProtocolError> {
        let position = self
            .pending_pubrel
            .iter()
            .position(|pending| pending.packet_id == packet_id)
            .ok_or(ProtocolError::BadIdentifier)?;
        self.pending_pubrel.swap_remove(position);
        Ok(())
    }

    fn republish_packets(&self) -> impl Iterator<Item = &[u8]> {
        self.entries
            .iter()
            .map(|entry| &self.buf[entry.offset..entry.offset + entry.len])
    }

    fn pending_pubrels(&self) -> impl Iterator<Item = PendingPubrel> + '_ {
        self.pending_pubrel.iter().copied()
    }

    fn mark_reconnect_pending(&mut self) {
        self.replay_pending = self.pending_transactions();
    }

    fn replay_pending(&self) -> bool {
        self.replay_pending
    }

    fn finish_replay(&mut self) {
        self.replay_pending = false;
    }

    fn compact(&mut self) {
        self.entries.sort_unstable_by_key(|entry| entry.offset);

        let mut cursor = 0;
        for entry in self.entries.iter_mut() {
            if entry.offset != cursor {
                self.buf
                    .copy_within(entry.offset..entry.offset + entry.len, cursor);
                entry.offset = cursor;
            }
            cursor += entry.len;
        }
        self.used = cursor;
    }
}

#[derive(Debug)]
struct SessionState<'a> {
    client_id: String<64>,
    packet_id: u16,
    outbound: Inflight<'a>,
    pending_server_packet_ids: Vec<u16, MAX_INBOUND_QOS2>,
    had_state: bool,
    was_reset: bool,
}

impl<'a> SessionState<'a> {
    fn new(client_id: String<64>, inflight: &'a mut [u8]) -> Self {
        Self {
            client_id,
            packet_id: 1,
            outbound: Inflight::new(inflight),
            pending_server_packet_ids: Vec::new(),
            had_state: false,
            was_reset: false,
        }
    }

    fn register_connected(&mut self) {
        self.had_state = true;
    }

    fn reset(&mut self) {
        self.was_reset = self.had_state;
        self.had_state = false;
        self.packet_id = 1;
        self.outbound.clear();
        self.pending_server_packet_ids.clear();
    }

    fn take_reset(&mut self) -> bool {
        let reset = self.was_reset;
        self.was_reset = false;
        reset
    }

    fn next_packet_id(&mut self) -> u16 {
        let packet_id = self.packet_id;
        self.packet_id = if self.packet_id == u16::MAX {
            1
        } else {
            self.packet_id + 1
        };
        packet_id
    }

    fn handshakes_pending(&self) -> bool {
        !self.pending_server_packet_ids.is_empty() || self.outbound.pending_transactions()
    }
}

#[derive(Debug)]
pub(super) struct Core<'buf> {
    broker: Broker,
    packet_reader: PacketReader<'buf>,
    tx_buffer: &'buf mut [u8],
    session: SessionState<'buf>,
    will: Option<Will<'buf>>,
    auth: Option<Auth<'buf>>,
    downgrade_qos: bool,
    keep_alive_interval: Duration,
    send_quota: u16,
    max_send_quota: u16,
    maximum_packet_size: Option<u32>,
    max_qos: Option<QoS>,
    pending_subscriptions: Vec<u16, MAX_PENDING_SUBSCRIPTIONS>,
    pub(super) state: State,
    next_ping: Option<Instant>,
    ping_timeout: Option<Instant>,
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
            packet_reader: PacketReader::new(buffers.rx),
            tx_buffer: buffers.tx,
            session: SessionState::new(client_id, buffers.inflight),
            will,
            auth,
            downgrade_qos,
            keep_alive_interval: keepalive_interval,
            send_quota: u16::MAX,
            max_send_quota: u16::MAX,
            maximum_packet_size: None,
            max_qos: None,
            pending_subscriptions: Vec::new(),
            state: State::Disconnected,
            next_ping: None,
            ping_timeout: None,
        }
    }

    pub(super) fn broker(&self) -> &Broker {
        &self.broker
    }

    pub(super) fn is_connected(&self) -> bool {
        self.state == State::Active
    }

    pub(super) fn subscriptions_pending(&self) -> bool {
        !self.pending_subscriptions.is_empty()
    }

    pub(super) fn pending_messages(&self) -> bool {
        self.session.handshakes_pending()
    }

    pub(super) fn can_publish(&mut self, qos: QoS) -> bool {
        if self.state != State::Active {
            return false;
        }
        if qos != QoS::AtMostOnce && self.send_quota == 0 {
            return false;
        }
        self.session.outbound.can_publish(self.tx_buffer.len())
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        match (self.next_ping, self.ping_timeout) {
            (Some(ping), Some(timeout)) => Some(ping.min(timeout)),
            (Some(ping), None) => Some(ping),
            (None, Some(timeout)) => Some(timeout),
            (None, None) => None,
        }
    }

    pub(super) async fn connect<C>(&mut self, connection: &mut C) -> Result<(), Error>
    where
        C: Read + Write + ErrorType,
        C::Error: embedded_io_async::Error,
    {
        let client_id = self.session.client_id.clone();
        let properties = [
            Property::MaximumPacketSize(self.packet_reader.buffer.len() as u32),
            Property::SessionExpiryInterval(u32::MAX),
            Property::ReceiveMaximum(self.session.pending_server_packet_ids.capacity() as u16),
        ];

        self.write_packet(
            connection,
            &Connect {
                keep_alive: self.keepalive_seconds(),
                properties: Properties::Slice(&properties),
                client_id: Utf8String(client_id.as_str()),
                auth: self.auth,
                will: self.will.clone(),
                clean_start: !self.session.had_state,
            },
        )
        .await?;

        self.state = State::Establishing;
        self.next_ping = Some(Instant::now() + self.keep_alive_interval / 2);
        self.ping_timeout = None;
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
        if self.state != State::Active {
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
        self.pending_subscriptions
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
        if self.state != State::Active {
            return Err(PubError::Error(Error::Disconnected));
        }

        let mut publish: Pub<'_, P> = publish.into();
        if let Some(max_qos) = self.max_qos
            && self.downgrade_qos
            && publish.qos > max_qos
        {
            publish.qos = max_qos;
        }

        if publish.qos > QoS::AtMostOnce && self.session.outbound.metadata_full() {
            return Err(PubError::Error(
                ProtocolError::InflightMetadataExhausted.into(),
            ));
        }

        if !self.can_publish(publish.qos) {
            return Err(PubError::Error(Error::NotReady));
        }

        publish.packet_id = (publish.qos > QoS::AtMostOnce).then(|| self.session.next_packet_id());
        publish.dup = false;

        let tx_ptr = self.tx_buffer.as_ptr() as usize;
        let packet_id = publish.packet_id;
        let qos = publish.qos;
        let (packet_offset, packet_len) = {
            let packet = self.serialize_publish(publish)?;
            if let Err(err) = connection.write_all(packet).await {
                self.handle_disconnect();
                return Err(PubError::Error(Error::Transport(err.kind())));
            }
            if let Err(err) = connection.flush().await {
                self.handle_disconnect();
                return Err(PubError::Error(Error::Transport(err.kind())));
            }
            let offset = packet.as_ptr() as usize - tx_ptr;
            (offset, packet.len())
        };

        if let Some(packet_id) = packet_id {
            let packet = &self.tx_buffer[packet_offset..packet_offset + packet_len];
            self.session
                .outbound
                .push_publish(packet_id, qos, packet)
                .map_err(|err| PubError::Error(err.into()))?;
            self.send_quota = self.send_quota.saturating_sub(1);
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
        if self.state != State::Active {
            return Ok(());
        }

        if self
            .ping_timeout
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
        {
            self.handle_disconnect();
            return Ok(());
        }

        if self.session.outbound.replay_pending() {
            for packet in self.session.outbound.republish_packets() {
                self.tx_buffer[..packet.len()].copy_from_slice(packet);
                self.tx_buffer[0] |= 1 << 3;
                connection
                    .write_all(&self.tx_buffer[..packet.len()])
                    .await
                    .map_err(|err| Error::Transport(err.kind()))?;
            }

            let pending_pubrels: Vec<PendingPubrel, MAX_PENDING_PUBREL> =
                self.session.outbound.pending_pubrels().collect();
            for pubrel in pending_pubrels {
                self.write_packet(
                    connection,
                    &PubRel {
                        packet_id: pubrel.packet_id,
                        reason: pubrel.reason.into(),
                    },
                )
                .await?;
            }

            self.session.outbound.finish_replay();
        }

        if self
            .next_ping
            .map(|deadline| now >= deadline)
            .unwrap_or(false)
        {
            self.write_packet(connection, &PingReq {}).await?;
            self.ping_timeout = Some(now + Duration::from_millis(PING_TIMEOUT_MS));
            self.next_ping = Some(now + self.keep_alive_interval / 2);
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
        if self.state == State::Disconnected {
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
                tx_buffer: self.tx_buffer,
                session: &mut self.session,
                pending_subscriptions: &mut self.pending_subscriptions,
                state: &mut self.state,
                keep_alive_interval: &mut self.keep_alive_interval,
                send_quota: &mut self.send_quota,
                max_send_quota: &mut self.max_send_quota,
                maximum_packet_size: &mut self.maximum_packet_size,
                max_qos: &mut self.max_qos,
                next_ping: &mut self.next_ping,
                ping_timeout: &mut self.ping_timeout,
            };
            let result = handle_packet(&mut ctx, connection, packet, now).await;
            let transport_error = matches!(result, Err(Error::Transport(_)));
            if transport_error {
                ctx.mark_disconnected();
            }
            (result, transport_error)
        };
        debug_assert!(!transport_error || self.state == State::Disconnected);
        result
    }

    fn handle_disconnect(&mut self) {
        self.session.outbound.mark_reconnect_pending();
        self.state = State::Disconnected;
        self.packet_reader.reset();
        self.pending_subscriptions.clear();
        self.next_ping = None;
        self.ping_timeout = None;
    }

    fn keepalive_seconds(&self) -> u16 {
        self.keep_alive_interval.as_secs() as u16
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
        write_packet(self.tx_buffer, connection, packet).await
    }

    fn serialize_publish<P: crate::publication::ToPayload>(
        &mut self,
        packet: Pub<'_, P>,
    ) -> Result<&[u8], PubError<P::Error>> {
        crate::ser::MqttSerializer::pub_to_buffer(self.tx_buffer, packet).map_err(|err| match err {
            crate::ser::PubError::Error(err) => PubError::Error(Error::Protocol(err.into())),
            crate::ser::PubError::Other(err) => PubError::Serialization(err),
        })
    }
}

struct PacketContext<'a, 'buf> {
    tx_buffer: &'a mut [u8],
    session: &'a mut SessionState<'buf>,
    pending_subscriptions: &'a mut Vec<u16, MAX_PENDING_SUBSCRIPTIONS>,
    state: &'a mut State,
    keep_alive_interval: &'a mut Duration,
    send_quota: &'a mut u16,
    max_send_quota: &'a mut u16,
    maximum_packet_size: &'a mut Option<u32>,
    max_qos: &'a mut Option<QoS>,
    next_ping: &'a mut Option<Instant>,
    ping_timeout: &'a mut Option<Instant>,
}

impl PacketContext<'_, '_> {
    fn mark_disconnected(&mut self) {
        self.session.outbound.mark_reconnect_pending();
        *self.state = State::Disconnected;
        self.pending_subscriptions.clear();
        *self.next_ping = None;
        *self.ping_timeout = None;
    }
}

async fn handle_packet<'pkt, 'state, C>(
    cx: &mut PacketContext<'_, 'state>,
    connection: &mut C,
    packet: ReceivedPacket<'pkt>,
    now: Instant,
) -> Result<Option<InboundPublish<'pkt>>, Error>
where
    C: Read + Write + ErrorType,
    C::Error: embedded_io_async::Error,
{
    match packet {
        ReceivedPacket::ConnAck(ack) => {
            ack.reason_code.as_result()?;
            if !ack.session_present {
                cx.session.reset();
                cx.pending_subscriptions.clear();
            }

            *cx.send_quota = cx.session.outbound.max_send_quota();
            *cx.max_send_quota = cx.session.outbound.max_send_quota();
            *cx.max_qos = None;
            *cx.maximum_packet_size = None;

            for property in ack.properties.into_iter() {
                match property? {
                    Property::MaximumPacketSize(size) => *cx.maximum_packet_size = Some(size),
                    Property::AssignedClientIdentifier(id) => {
                        cx.session.client_id = String::try_from(id.0)
                            .map_err(|_| ProtocolError::ProvidedClientIdTooLong)?;
                    }
                    Property::ServerKeepAlive(keep_alive) => {
                        *cx.keep_alive_interval = Duration::from_secs(keep_alive as u64);
                    }
                    Property::ReceiveMaximum(max) => {
                        let local = cx.session.outbound.max_send_quota();
                        *cx.send_quota = max.min(local);
                        *cx.max_send_quota = max.min(local);
                    }
                    Property::MaximumQoS(max) => {
                        *cx.max_qos =
                            Some(QoS::try_from(max).map_err(|_| ProtocolError::WrongQos)?);
                    }
                    _ => {}
                }
            }

            *cx.state = State::Active;
            cx.session.register_connected();
            *cx.next_ping = Some(now + *cx.keep_alive_interval / 2);
            *cx.ping_timeout = None;
            if cx.session.take_reset() {
                return Err(Error::SessionReset);
            }
        }
        ReceivedPacket::SubAck(ack) => {
            let index = cx
                .pending_subscriptions
                .iter()
                .position(|id| *id == ack.packet_identifier)
                .ok_or(ProtocolError::BadIdentifier)?;
            cx.pending_subscriptions.swap_remove(index);
            for &code in ack.codes {
                ReasonCode::from(code).as_result()?;
            }
        }
        ReceivedPacket::PingResp => {
            *cx.ping_timeout = None;
        }
        ReceivedPacket::PubAck(ack) => {
            *cx.send_quota = cx.send_quota.saturating_add(1).min(*cx.max_send_quota);
            cx.session.outbound.remove_publish(ack.packet_identifier)?;
        }
        ReceivedPacket::PubRec(rec) => {
            rec.reason.code().as_result()?;
            *cx.send_quota = cx.send_quota.saturating_add(1).min(*cx.max_send_quota);
            cx.session.outbound.remove_publish(rec.packet_id)?;
            cx.session
                .outbound
                .queue_pubrel(rec.packet_id, ReasonCode::Success)?;
            write_packet(
                cx.tx_buffer,
                connection,
                &PubRel {
                    packet_id: rec.packet_id,
                    reason: ReasonCode::Success.into(),
                },
            )
            .await?;
        }
        ReceivedPacket::PubComp(comp) => {
            cx.session.outbound.remove_pubrel(comp.packet_id)?;
        }
        ReceivedPacket::PubRel(rel) => {
            let reason = if let Some(index) = cx
                .session
                .pending_server_packet_ids
                .iter()
                .position(|id| *id == rel.packet_id)
            {
                cx.session.pending_server_packet_ids.swap_remove(index);
                ReasonCode::Success
            } else {
                ReasonCode::PacketIdNotFound
            };
            write_packet(
                cx.tx_buffer,
                connection,
                &PubComp {
                    packet_id: rel.packet_id,
                    reason: reason.into(),
                },
            )
            .await?;
        }
        ReceivedPacket::Publish(info) => {
            let retain = info.retain;
            let qos = info.qos;
            match info.qos {
                QoS::AtMostOnce => {}
                QoS::AtLeastOnce => {
                    let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                    let reason = if cx.session.pending_server_packet_ids.contains(&packet_id) {
                        ReasonCode::PacketIdInUse
                    } else {
                        ReasonCode::Success
                    };
                    write_packet(
                        cx.tx_buffer,
                        connection,
                        &PubAck {
                            packet_identifier: packet_id,
                            reason: reason.into(),
                        },
                    )
                    .await?;
                }
                QoS::ExactlyOnce => {
                    let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                    let duplicate = cx.session.pending_server_packet_ids.contains(&packet_id);
                    let reason = if !duplicate {
                        cx.session
                            .pending_server_packet_ids
                            .push(packet_id)
                            .map(|_| ReasonCode::Success)
                            .unwrap_or(ReasonCode::ReceiveMaxExceeded)
                    } else {
                        ReasonCode::Success
                    };
                    write_packet(
                        cx.tx_buffer,
                        connection,
                        &PubRec {
                            packet_id,
                            reason: reason.into(),
                        },
                    )
                    .await?;
                    if duplicate || !reason.success() {
                        return Ok(None);
                    }
                }
            }

            *cx.next_ping = Some(now + *cx.keep_alive_interval / 2);
            return Ok(Some(InboundPublish {
                topic: info.topic.0,
                payload: info.payload,
                properties: info.properties,
                retain,
                qos,
            }));
        }
        ReceivedPacket::Disconnect(_) => {
            cx.mark_disconnected();
        }
    }

    Ok(None)
}

async fn write_packet<C, T>(
    tx_buffer: &mut [u8],
    connection: &mut C,
    packet: &T,
) -> Result<(), Error>
where
    C: Read + Write + ErrorType,
    C::Error: embedded_io_async::Error,
    T: serde::Serialize + crate::message_types::ControlPacket + core::fmt::Debug,
{
    let bytes = crate::ser::MqttSerializer::to_buffer(tx_buffer, packet)
        .map_err(|err| Error::Protocol(err.into()))?;
    connection
        .write_all(bytes)
        .await
        .map_err(|err| Error::Transport(err.kind()))?;
    connection
        .flush()
        .await
        .map_err(|err| Error::Transport(err.kind()))?;
    Ok(())
}
