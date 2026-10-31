use crate::packets::PublishHeader;
use crate::publication::ToPayload;
use crate::ser::{Error as SerError, MAX_FIXED_HEADER_SIZE, MqttSerializer};
use crate::wire::{ControlPacket, mark_publish_duplicate};
use crate::{Error, ProtocolError, PubError, ReasonCode, ResourceError, error, trace};
use heapless::Vec;

use super::Io;

pub(super) const MAX_RETAINED: usize = 8;
pub(super) const MAX_PENDING_CONTROL: usize = 8;

#[derive(Debug, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) enum ControlAction {
    PubAck { packet_id: u16, reason: ReasonCode },
    PubRec { packet_id: u16, reason: ReasonCode },
    PubRel { packet_id: u16 },
    PubComp { packet_id: u16, reason: ReasonCode },
    PingReq,
}

impl ControlAction {
    fn encoded_len(self, protocol: crate::Protocol) -> usize {
        match self {
            Self::PingReq => 2,
            Self::PubAck { reason, .. }
            | Self::PubRec { reason, .. }
            | Self::PubComp { reason, .. }
                if protocol.is_v5() && reason != ReasonCode::Success =>
            {
                5
            }
            _ => 4,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq)]
struct PendingControl {
    action: ControlAction,
    state: SendState,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum PacketKind {
    PublishAtLeastOnce,
    PublishExactlyOnce,
    Subscribe,
    Unsubscribe,
    PubRel,
}

impl PacketKind {
    fn is_publish(self) -> bool {
        matches!(self, Self::PublishAtLeastOnce | Self::PublishExactlyOnce)
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct RetainedPacket {
    kind: PacketKind,
    packet_id: u16,
    len: usize,
    response_count: usize,
    state: SendState,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum SendState {
    // A PUBLISH has not yet consumed this connection's send quota.
    Queued,
    Write { written: usize },
    Flush,
    Sent,
}

impl SendState {
    fn set_written(&mut self, written: usize, len: usize) {
        *self = if written >= len {
            Self::Flush
        } else {
            Self::Write { written }
        };
    }

    fn write_offset(self, in_progress: bool, can_start: bool) -> Option<usize> {
        match self {
            Self::Queued if !in_progress && can_start => Some(0),
            Self::Write { written } if (written != 0) == in_progress => Some(written),
            _ => None,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) struct RetainedStep {
    pub(super) packet_id: u16,
    pub(super) offset: usize,
    pub(super) len: usize,
    pub(super) written: usize,
    pub(super) charge_quota: bool,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) struct ReleaseStep {
    pub(super) packet_id: u16,
    pub(super) written: usize,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) struct ControlStep {
    pub(super) index: usize,
    pub(super) action: ControlAction,
    pub(super) written: usize,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) enum OutboundStep {
    Control(ControlStep),
    Release(ReleaseStep),
    Retained(RetainedStep),
}

#[derive(Debug)]
pub(super) struct Outbound<'a> {
    buf: &'a mut [u8],
    connect_workspace: usize,
    pending_control: Vec<PendingControl, MAX_PENDING_CONTROL>,
    retained: Vec<RetainedPacket, MAX_RETAINED>,
}

impl<'a> Outbound<'a> {
    pub(super) fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            connect_workspace: 0,
            pending_control: Vec::new(),
            retained: Vec::new(),
        }
    }

    pub(super) fn clear_inflight(&mut self) {
        self.pending_control.clear();
        self.retained.clear();
    }

    pub(super) fn is_quiescent(&self) -> bool {
        self.pending_control.is_empty() && self.retained.is_empty()
    }

    pub(super) fn retained_full(&self) -> bool {
        self.retained.is_full()
    }

    pub(super) fn retained_bytes(&self) -> usize {
        self.retained.iter().map(|entry| entry.len).sum()
    }

    pub(super) fn capacity(&self) -> usize {
        self.buf.len()
    }

    pub(super) fn retained_len(&self) -> usize {
        self.retained.len()
    }

    pub(super) fn pending_control_len(&self) -> usize {
        self.pending_control.len()
    }

    pub(super) fn pending_release_len(&self) -> usize {
        self.retained
            .iter()
            .filter(|entry| entry.kind == PacketKind::PubRel)
            .count()
    }

    pub(super) fn max_inflight(&self) -> u16 {
        MAX_RETAINED as u16
    }

    /// Queued publications reserve admission capacity until their first write
    /// charges the connection's send quota. Replay uses the same path.
    pub(super) fn queued_publishes(&self) -> u16 {
        self.retained
            .iter()
            .filter(|entry| entry.kind.is_publish() && entry.state == SendState::Queued)
            .count() as u16
    }

    pub(super) fn scratch_len(&self) -> usize {
        self.buf.len().saturating_sub(self.retained_bytes())
    }

    pub(super) fn can_retain(&self) -> bool {
        self.retained.len() < self.retained.capacity()
            && self
                .retained_capacity()
                .saturating_sub(self.retained_bytes())
                >= MAX_FIXED_HEADER_SIZE
    }

    pub(super) fn scratch_space(&mut self) -> &mut [u8] {
        let used = self.retained_bytes();
        &mut self.buf[used..]
    }

    pub(super) fn set_connect_workspace(&mut self, len: usize) -> Result<(), ResourceError> {
        let capacity = self
            .buf
            .len()
            .checked_sub(len)
            .ok_or(ResourceError::BufferTooSmall)?;
        if self.retained_bytes() > capacity {
            return Err(ResourceError::BufferTooSmall);
        }
        self.connect_workspace = len;
        Ok(())
    }

    pub(super) fn queue_control(&mut self, action: ControlAction) -> Result<(), ProtocolError> {
        self.pending_control
            .push(PendingControl {
                action,
                state: SendState::Write { written: 0 },
            })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)
    }

    pub(super) fn has_pending_pingreq(&self) -> bool {
        self.pending_control.iter().any(|entry| {
            matches!(entry.action, ControlAction::PingReq) && entry.state != SendState::Sent
        })
    }

    pub(super) fn ack_packet(&mut self, kind: PacketKind, packet_id: u16) -> bool {
        let Some(position) = self
            .retained
            .iter()
            .position(|entry| entry.kind == kind && entry.packet_id == packet_id)
        else {
            return false;
        };
        self.remove_bytes(position);
        self.retained.remove(position);
        true
    }

    pub(super) fn ack_subscription(
        &mut self,
        kind: PacketKind,
        packet_id: u16,
        codes: &[u8],
        protocol: crate::Protocol,
    ) -> Result<bool, ProtocolError> {
        use serde::Deserialize;
        let Some(position) = self
            .retained
            .iter()
            .position(|entry| entry.kind == kind && entry.packet_id == packet_id)
        else {
            return Ok(false);
        };
        let entry = &self.retained[position];
        if (kind == PacketKind::Subscribe || protocol.is_v5())
            && entry.response_count != codes.len()
        {
            return Err(ProtocolError::MalformedPacket);
        }
        if kind == PacketKind::Subscribe {
            let offset: usize = self.retained[..position]
                .iter()
                .map(|entry| entry.len)
                .sum();
            let mut decoder =
                crate::de::MqttDeserializer::new(&self.buf[offset..offset + entry.len]);
            u8::deserialize(&mut decoder)?;
            crate::varint::Varint::deserialize(&mut decoder)?;
            u16::deserialize(&mut decoder)?;
            if protocol.is_v5() {
                crate::Properties::deserialize(&mut decoder)?;
            }
            for &code in codes {
                crate::wire::Utf8String::deserialize(&mut decoder)?;
                let options = u8::deserialize(&mut decoder)?;
                if code <= 2 && code > options & 3 {
                    return Err(ProtocolError::MalformedPacket);
                }
            }
        }
        self.remove_bytes(position);
        self.retained.remove(position);
        Ok(true)
    }

    fn remove_bytes(&mut self, position: usize) {
        let len = self.retained[position].len;
        if len == 0 {
            return;
        }
        let offset = self.retained[..position]
            .iter()
            .map(|entry| entry.len)
            .sum::<usize>();
        let used = self.retained_bytes();
        self.buf.copy_within(offset + len..used, offset);
    }

    pub(super) fn has_retained(&self, packet_id: u16) -> bool {
        self.retained
            .iter()
            .any(|entry| entry.packet_id == packet_id)
    }

    /// Keep the original slot through PUBCOMP; only the encoded PUBLISH bytes
    /// are released. Moving the entry to the tail preserves PUBREL send order
    /// when PUBRECs arrive out of order, including across reconnects.
    pub(super) fn release(&mut self, packet_id: u16) -> bool {
        let Some(position) = self
            .retained
            .iter()
            .position(|entry| entry.packet_id == packet_id)
        else {
            return false;
        };
        match self.retained[position].kind {
            PacketKind::PublishExactlyOnce => {
                self.remove_bytes(position);
                let entry = &mut self.retained[position];
                entry.kind = PacketKind::PubRel;
                entry.len = 0;
                entry.state = SendState::Queued;
                self.retained[position..].rotate_left(1);
            }
            PacketKind::PubRel => {
                // A duplicate PUBREC requests another PUBREL. Do not restart a
                // partial write or duplicate a response already queued for flush.
                if self.retained[position].state == SendState::Sent {
                    self.retained[position].state = SendState::Queued;
                }
            }
            _ => return false,
        }
        true
    }

    fn mark_publish_dup(&mut self) {
        let mut offset = 0;
        for entry in &self.retained {
            if entry.kind.is_publish() {
                mark_publish_duplicate(&mut self.buf[offset]);
            }
            offset += entry.len;
        }
    }

    pub(super) fn encode_publish<P: ToPayload, E>(
        &mut self,
        header: &PublishHeader<'_>,
        payload: P,
    ) -> Result<usize, PubError<P::Error, E>> {
        let start = self.retained_bytes();
        // Adaptive payloads must see the retained budget, including encoder workspace.
        let end = self.retained_capacity();
        let (offset, packet) =
            MqttSerializer::encode_publish_with_offset(&mut self.buf[start..end], header, payload)?;
        let len = packet.len();
        self.pack_encoded(offset, len)
            .map_err(|err| PubError::Session(Error::Resource(err)))?;
        Ok(len)
    }

    pub(super) fn encode_packet<T>(&mut self, packet: &T) -> Result<usize, ProtocolError>
    where
        T: serde::Serialize + ControlPacket,
    {
        let start = self.retained_bytes();
        let (offset, packet) = MqttSerializer::encode_with_offset(&mut self.buf[start..], packet)?;
        let len = packet.len();
        self.pack_encoded(offset, len)
            .map_err(|_| SerError::InsufficientMemory)?;
        Ok(len)
    }

    pub(super) fn retained_packet(&self, offset: usize, len: usize) -> &[u8] {
        &self.buf[offset..offset + len]
    }

    pub(super) fn retain_packet(
        &mut self,
        kind: PacketKind,
        packet_id: u16,
        len: usize,
        response_count: usize,
    ) -> Result<(), ProtocolError> {
        self.retained
            .push(RetainedPacket {
                kind,
                packet_id,
                len,
                response_count,
                state: SendState::Queued,
            })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)?;
        Ok(())
    }

    pub(super) fn complete_flush(&mut self) {
        self.pending_control.retain(|p| p.state != SendState::Flush);
        for p in &mut self.retained {
            if p.state == SendState::Flush {
                p.state = SendState::Sent;
            }
        }
    }

    pub(super) fn next_step(&self, can_publish: bool) -> Option<OutboundStep> {
        for in_progress in [true, false] {
            for (index, entry) in self.pending_control.iter().enumerate() {
                if let Some(written) = entry.state.write_offset(in_progress, true) {
                    return Some(OutboundStep::Control(ControlStep {
                        index,
                        action: entry.action,
                        written,
                    }));
                }
            }
            for entry in &self.retained {
                if entry.kind == PacketKind::PubRel
                    && let Some(written) = entry.state.write_offset(in_progress, true)
                {
                    return Some(OutboundStep::Release(ReleaseStep {
                        packet_id: entry.packet_id,
                        written,
                    }));
                }
            }
            let mut offset = 0;
            for entry in &self.retained {
                if entry.kind != PacketKind::PubRel
                    && let Some(written) = entry
                        .state
                        .write_offset(in_progress, can_publish || !entry.kind.is_publish())
                {
                    return Some(OutboundStep::Retained(RetainedStep {
                        packet_id: entry.packet_id,
                        offset,
                        len: entry.len,
                        written,
                        charge_quota: entry.kind.is_publish() && entry.state == SendState::Queued,
                    }));
                }
                offset += entry.len;
            }
        }
        None
    }

    pub(super) fn set_control_written(&mut self, index: usize, written: usize, len: usize) -> bool {
        if let Some(entry) = self.pending_control.get_mut(index) {
            entry.state.set_written(written, len);
            true
        } else {
            false
        }
    }

    pub(super) fn set_retained_written(
        &mut self,
        packet_id: u16,
        written: usize,
        len: usize,
    ) -> bool {
        if let Some(entry) = self
            .retained
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state.set_written(written, len);
            true
        } else {
            false
        }
    }

    pub(super) fn arm_replay(&mut self) {
        if self.is_quiescent() {
            return;
        }

        trace!(
            "Arming outbound replay control={=usize} retained={=usize} pending_release={=usize} tx_used={=usize} tx_capacity={=usize}",
            self.pending_control.len(),
            self.retained.len(),
            self.pending_release_len(),
            self.retained_bytes(),
            self.buf.len()
        );
        self.mark_publish_dup();
        for entry in &mut self.pending_control {
            entry.state = SendState::Write { written: 0 };
        }
        for entry in &mut self.retained {
            entry.state = SendState::Queued;
        }
    }

    /// Check the complete backlog before exposing a connection with new limits.
    pub(super) fn validate_replay<E>(
        &self,
        protocol: crate::Protocol,
        maximum_packet_size: Option<u32>,
        max_qos: Option<crate::QoS>,
        capabilities: super::Capabilities,
    ) -> Result<(), Error<E>> {
        use serde::Deserialize;
        for control in &self.pending_control {
            check_control_packet_size(maximum_packet_size, control.action, protocol)?;
        }
        let mut offset = 0;
        for entry in &self.retained {
            let len = if entry.kind == PacketKind::PubRel {
                4
            } else {
                entry.len
            };
            if maximum_packet_size.is_some_and(|max| len > max as usize) {
                return Err(ResourceError::PacketTooLarge.into());
            }
            let bytes = &self.buf[offset..offset + entry.len];
            offset += entry.len;
            if entry.kind.is_publish() {
                let qos = if entry.kind == PacketKind::PublishExactlyOnce {
                    crate::QoS::ExactlyOnce
                } else {
                    crate::QoS::AtLeastOnce
                };
                if max_qos.is_some_and(|max| qos > max)
                    || (!capabilities.retain && bytes[0] & 1 != 0)
                {
                    return Err(Error::InvalidRequest);
                }
            } else if entry.kind == PacketKind::Subscribe {
                let mut decoder = crate::de::MqttDeserializer::new(bytes);
                let mut check = || -> Result<bool, crate::DeError> {
                    u8::deserialize(&mut decoder)?;
                    crate::varint::Varint::deserialize(&mut decoder)?;
                    u16::deserialize(&mut decoder)?;
                    if protocol.is_v5() {
                        let properties = crate::Properties::deserialize(&mut decoder)?;
                        if !capabilities.subscription_ids
                            && properties.iter().any(|p| {
                                matches!(p, Ok(crate::Property::SubscriptionIdentifier(_)))
                            })
                        {
                            return Ok(false);
                        }
                    }
                    for _ in 0..entry.response_count {
                        let topic = crate::wire::Utf8String::deserialize(&mut decoder)?;
                        let options = u8::deserialize(&mut decoder)?;
                        if !capabilities.allows_filter(topic.0, options & 4 != 0) {
                            return Ok(false);
                        }
                    }
                    Ok(true)
                };
                if !check()? {
                    return Err(Error::InvalidRequest);
                }
            }
        }
        Ok(())
    }

    fn pack_encoded(&mut self, offset: usize, len: usize) -> Result<(), ResourceError> {
        let start = self.retained_bytes();
        if len > self.retained_capacity().saturating_sub(start) {
            return Err(ResourceError::BufferTooSmall);
        }
        self.buf
            .copy_within(start + offset..start + offset + len, start);
        Ok(())
    }

    fn retained_capacity(&self) -> usize {
        self.buf.len() - self.connect_workspace
    }
}

pub(crate) fn serialize_control_packet<E>(
    buffer: &mut [u8; 5],
    packet: ControlAction,
    maximum_packet_size: Option<u32>,
    protocol: crate::Protocol,
) -> Result<&[u8], Error<E>> {
    let bytes = encode_control_packet(buffer, packet, protocol);
    if maximum_packet_size.is_some_and(|max| bytes.len() > max as usize) {
        return Err(Error::Resource(ResourceError::PacketTooLarge));
    }
    Ok(bytes)
}

fn encode_control_packet(
    buffer: &mut [u8; 5],
    packet: ControlAction,
    protocol: crate::Protocol,
) -> &[u8] {
    let (header, packet_id, reason) = match packet {
        ControlAction::PubAck { packet_id, reason } => (0x40, packet_id, reason),
        ControlAction::PubRec { packet_id, reason } => (0x50, packet_id, reason),
        ControlAction::PubRel { packet_id } => (0x62, packet_id, ReasonCode::Success),
        ControlAction::PubComp { packet_id, reason } => (0x70, packet_id, reason),
        ControlAction::PingReq => {
            buffer[..2].copy_from_slice(&[0xc0, 0]);
            return &buffer[..2];
        }
    };
    let len = packet.encoded_len(protocol);
    let [msb, lsb] = packet_id.to_be_bytes();
    *buffer = [header, (len - 2) as u8, msb, lsb, reason as u8];
    &buffer[..len]
}

pub(super) fn check_control_packet_size(
    maximum_packet_size: Option<u32>,
    action: ControlAction,
    protocol: crate::Protocol,
) -> Result<(), ProtocolError> {
    let len = action.encoded_len(protocol);
    if maximum_packet_size.is_some_and(|max| len > max as usize) {
        return Err(ProtocolError::PacketTooLarge);
    }
    Ok(())
}

pub(super) async fn write_all<C: Io>(
    connection: &mut C,
    mut bytes: &[u8],
) -> Result<(), Error<C::Error>> {
    while !bytes.is_empty() {
        let written = connection.write(bytes).await.map_err(Error::Transport)?;
        if written == 0 {
            error!("transport write returned zero bytes for non-empty buffer");
            return Err(Error::WriteZero);
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ControlAction, MAX_FIXED_HEADER_SIZE, Outbound, OutboundStep, PacketKind};
    use crate::{
        Error, Properties, PubError, ReasonCode, ResourceError,
        packets::{PublishHeader, Subscribe},
        publication::Publication,
        types::TopicFilter,
        wire::Utf8String,
    };

    #[test]
    fn control_packets_match_wire_fixtures_and_packet_limits() {
        use super::{check_control_packet_size, serialize_control_packet};
        use crate::{Protocol, ProtocolError};
        let cases: &[(ControlAction, &[u8], &[u8])] = &[
            (ControlAction::PingReq, &[0xc0, 0], &[0xc0, 0]),
            (
                ControlAction::PubAck {
                    packet_id: 0x1234,
                    reason: ReasonCode::Success,
                },
                &[0x40, 2, 0x12, 0x34],
                &[0x40, 2, 0x12, 0x34],
            ),
            (
                ControlAction::PubAck {
                    packet_id: 0x1234,
                    reason: ReasonCode::NotAuthorized,
                },
                &[0x40, 3, 0x12, 0x34, 0x87],
                &[0x40, 2, 0x12, 0x34],
            ),
            (
                ControlAction::PubRec {
                    packet_id: 0x1234,
                    reason: crate::ReasonCode::Success,
                },
                &[0x50, 2, 0x12, 0x34],
                &[0x50, 2, 0x12, 0x34],
            ),
            (
                ControlAction::PubRel { packet_id: 0x1234 },
                &[0x62, 2, 0x12, 0x34],
                &[0x62, 2, 0x12, 0x34],
            ),
            (
                ControlAction::PubComp {
                    packet_id: 0x1234,
                    reason: ReasonCode::Success,
                },
                &[0x70, 2, 0x12, 0x34],
                &[0x70, 2, 0x12, 0x34],
            ),
            (
                ControlAction::PubComp {
                    packet_id: 0x1234,
                    reason: ReasonCode::PacketIdNotFound,
                },
                &[0x70, 3, 0x12, 0x34, 0x92],
                &[0x70, 2, 0x12, 0x34],
            ),
        ];
        for &(action, v5, v3) in cases {
            for (protocol, expected) in [
                (Protocol::V5, v5),
                (
                    Protocol::V3_1_1 {
                        clean_session: false,
                    },
                    v3,
                ),
            ] {
                let mut buffer = [0; 5];
                let limit = expected.len() as u32;
                assert_eq!(
                    serialize_control_packet::<()>(&mut buffer, action, Some(limit), protocol)
                        .unwrap(),
                    expected
                );
                assert_eq!(
                    check_control_packet_size(Some(limit), action, protocol),
                    Ok(())
                );
                assert_eq!(
                    serialize_control_packet::<()>(&mut buffer, action, Some(limit - 1), protocol),
                    Err(Error::Resource(ResourceError::PacketTooLarge))
                );
                assert_eq!(
                    check_control_packet_size(Some(limit - 1), action, protocol),
                    Err(ProtocolError::PacketTooLarge)
                );
            }
        }
    }

    #[test]
    fn encoded_packet_is_packed_after_retained_prefix() {
        let mut storage = [0u8; 64];
        let mut outbound = Outbound::new(&mut storage);

        outbound
            .retain_packet(PacketKind::Subscribe, 7, 10, 0)
            .unwrap();

        let len = outbound
            .encode_packet(&Subscribe {
                packet_id: 16,
                properties: Some(Properties::from_slice(&[])),
                topics: &[TopicFilter::new("ABC")],
            })
            .unwrap();

        assert_eq!(&outbound.retained_packet(10, len)[..2], &[0x82, 0x09]);
    }

    #[test]
    fn can_retain_requires_fixed_header_scratch() {
        let mut storage = [0u8; MAX_FIXED_HEADER_SIZE + 4];
        let mut outbound = Outbound::new(&mut storage);

        outbound
            .retain_packet(PacketKind::Subscribe, 7, 5, 0)
            .unwrap();

        assert!(!outbound.can_retain());
    }

    #[test]
    fn encode_publish_returns_insufficient_memory_when_only_header_gap_remains() {
        let mut storage = [0u8; MAX_FIXED_HEADER_SIZE + 4];
        let mut outbound = Outbound::new(&mut storage);

        outbound
            .retain_packet(PacketKind::Subscribe, 7, 5, 0)
            .unwrap();

        let publication = Publication::bytes("a", b"x");
        let header = PublishHeader {
            topic: Utf8String(publication.topic),
            packet_id: None,
            properties: Some(publication.properties),
            retain: publication.retain,
            qos: publication.qos,
            dup: false,
        };
        let result = outbound.encode_publish::<_, ()>(&header, publication.payload);

        assert!(matches!(
            result,
            Err(PubError::Session(Error::Resource(
                ResourceError::BufferTooSmall
            )))
        ));
    }

    #[test]
    fn arm_replay_restarts_pending_control_from_byte_zero() {
        let mut storage = [0u8; 32];
        let mut outbound = Outbound::new(&mut storage);
        let action = ControlAction::PubAck {
            packet_id: 7,
            reason: ReasonCode::Success,
        };
        outbound.queue_control(action).unwrap();
        outbound.set_control_written(0, 5, 5);

        outbound.arm_replay();

        assert!(matches!(
            outbound.next_step(true),
            Some(OutboundStep::Control(step))
                if step.action == action && step.written == 0
        ));
    }

    #[test]
    fn acknowledgement_must_match_retained_operation() {
        let mut storage = [0u8; 16];
        let mut outbound = Outbound::new(&mut storage);
        outbound
            .retain_packet(PacketKind::Subscribe, 7, 5, 0)
            .unwrap();

        assert!(!outbound.ack_packet(PacketKind::PublishAtLeastOnce, 7));
        assert!(outbound.has_retained(7));
        assert!(outbound.ack_packet(PacketKind::Subscribe, 7));
        assert!(!outbound.has_retained(7));
    }
}
