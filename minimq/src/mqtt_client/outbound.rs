use crate::packets::{PingReq, PubAck, PubComp, PubRec, PubRel, PublishHeader};
use crate::publication::ToPayload;
use crate::ser::{Error as SerError, MAX_FIXED_HEADER_SIZE, MqttSerializer};
use crate::wire::{ControlPacket, mark_publish_duplicate};
use crate::{Error, ProtocolError, PubError, ReasonCode, ResourceError, error, trace};
use heapless::Vec;

use super::{Io, OpKind};

/// Serializer workspace for a fixed header and a four-byte acknowledgement body.
pub(super) const CONTROL_PACKET_WORKSPACE: usize = MAX_FIXED_HEADER_SIZE + 4;
pub(super) const MAX_RETAINED: usize = 8;
pub(super) const MAX_PENDING_CONTROL: usize = 8;
pub(super) const MAX_PENDING_RELEASE: usize = 8;

#[derive(Debug, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(super) enum ControlAction {
    PubAck { packet_id: u16, reason: ReasonCode },
    PubRec { packet_id: u16, reason: ReasonCode },
    PubComp { packet_id: u16, reason: ReasonCode },
    PingReq,
}

#[derive(Debug, Copy, Clone, PartialEq)]
struct PendingControl {
    action: ControlAction,
    state: SendState,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) struct PendingRelease {
    pub(super) packet_id: u16,
    pub(super) reason: ReasonCode,
    state: SendState,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct RetainedPacket {
    kind: OpKind,
    packet_id: u16,
    len: usize,
    state: SendState,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) enum SendState {
    Write { written: usize },
    Flush,
    Sent,
}

impl SendState {
    fn is_fresh(self) -> bool {
        matches!(self, Self::Write { written: 0 })
    }

    fn is_in_progress(self) -> bool {
        matches!(self, Self::Write { written: 1.. } | Self::Flush)
    }

    fn set_written(&mut self, written: usize, len: usize) {
        *self = if written >= len {
            Self::Flush
        } else {
            Self::Write { written }
        };
    }

    fn matches_priority(self, in_progress: bool) -> bool {
        if in_progress {
            self.is_in_progress()
        } else {
            self.is_fresh()
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(super) struct RetainedStep {
    pub(super) packet_id: u16,
    pub(super) offset: usize,
    pub(super) len: usize,
    pub(super) state: SendState,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) struct ReleaseStep {
    pub(super) packet_id: u16,
    pub(super) reason: ReasonCode,
    pub(super) state: SendState,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) struct ControlStep {
    pub(super) action: ControlAction,
    pub(super) state: SendState,
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
    pending_release: Vec<PendingRelease, MAX_PENDING_RELEASE>,
}

impl<'a> Outbound<'a> {
    pub(super) fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            connect_workspace: 0,
            pending_control: Vec::new(),
            retained: Vec::new(),
            pending_release: Vec::new(),
        }
    }

    pub(super) fn clear_inflight(&mut self) {
        self.pending_control.clear();
        self.retained.clear();
        self.pending_release.clear();
    }

    fn has_pending_state(&self) -> bool {
        !self.pending_control.is_empty()
            || !self.retained.is_empty()
            || !self.pending_release.is_empty()
    }

    pub(super) fn is_quiescent(&self) -> bool {
        !self.has_pending_state()
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
        self.pending_release.len()
    }

    pub(super) fn max_inflight(&self) -> u16 {
        MAX_RETAINED.min(MAX_PENDING_RELEASE) as u16
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

    pub(super) fn ack_packet(&mut self, kind: OpKind, packet_id: u16) -> bool {
        let Some(position) = self
            .retained
            .iter()
            .position(|entry| entry.kind == kind && entry.packet_id == packet_id)
        else {
            return false;
        };
        let offset = self.retained[..position]
            .iter()
            .map(|entry| entry.len)
            .sum::<usize>();
        let len = self.retained[position].len;
        let used = self.retained_bytes();
        self.buf.copy_within(offset + len..used, offset);
        self.retained.remove(position);
        true
    }

    pub(super) fn has_retained(&self, packet_id: u16) -> bool {
        self.retained
            .iter()
            .any(|entry| entry.packet_id == packet_id)
    }

    pub(super) fn queue_release(
        &mut self,
        packet_id: u16,
        reason: ReasonCode,
    ) -> Result<(), ProtocolError> {
        self.pending_release
            .push(PendingRelease {
                packet_id,
                reason,
                state: SendState::Write { written: 0 },
            })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)
    }

    pub(super) fn ack_release(&mut self, packet_id: u16) -> bool {
        let Some(position) = self
            .pending_release
            .iter()
            .position(|pending| pending.packet_id == packet_id)
        else {
            return false;
        };
        self.pending_release.swap_remove(position);
        true
    }

    pub(super) fn has_pending_release(&self, packet_id: u16) -> bool {
        self.pending_release
            .iter()
            .any(|pending| pending.packet_id == packet_id)
    }

    fn mark_publish_dup(&mut self) {
        let mut offset = 0;
        for entry in &self.retained {
            if matches!(
                entry.kind,
                OpKind::PublishAtLeastOnce | OpKind::PublishExactlyOnce
            ) {
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
        let (offset, packet) =
            MqttSerializer::encode_publish_with_offset(&mut self.buf[start..], header, payload)?;
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
        kind: OpKind,
        packet_id: u16,
        len: usize,
    ) -> Result<(), ProtocolError> {
        self.retained
            .push(RetainedPacket {
                kind,
                packet_id,
                len,
                state: SendState::Write { written: 0 },
            })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)?;
        Ok(())
    }

    pub(super) fn next_step(&self) -> Option<OutboundStep> {
        for in_progress in [true, false] {
            for entry in &self.pending_control {
                if entry.state.matches_priority(in_progress) {
                    return Some(OutboundStep::Control(ControlStep {
                        action: entry.action,
                        state: entry.state,
                    }));
                }
            }
            for entry in &self.pending_release {
                if entry.state.matches_priority(in_progress) {
                    return Some(OutboundStep::Release(ReleaseStep {
                        packet_id: entry.packet_id,
                        reason: entry.reason,
                        state: entry.state,
                    }));
                }
            }
            let mut offset = 0;
            for entry in &self.retained {
                if entry.state.matches_priority(in_progress) {
                    return Some(OutboundStep::Retained(RetainedStep {
                        packet_id: entry.packet_id,
                        offset,
                        len: entry.len,
                        state: entry.state,
                    }));
                }
                offset += entry.len;
            }
        }
        None
    }

    pub(super) fn set_control_written(
        &mut self,
        action: ControlAction,
        written: usize,
        len: usize,
    ) -> bool {
        if let Some(entry) = self
            .pending_control
            .iter_mut()
            .find(|entry| entry.action == action)
        {
            entry.state.set_written(written, len);
            true
        } else {
            false
        }
    }

    pub(super) fn flush_control(&mut self, action: ControlAction) -> bool {
        let found = if let Some(entry) = self
            .pending_control
            .iter_mut()
            .find(|entry| entry.action == action)
        {
            entry.state = SendState::Sent;
            true
        } else {
            false
        };
        self.pending_control
            .retain(|entry| entry.state != SendState::Sent);
        found
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

    pub(super) fn flush_retained(&mut self, packet_id: u16) -> bool {
        if let Some(entry) = self
            .retained
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state = SendState::Sent;
            true
        } else {
            false
        }
    }

    pub(super) fn set_release_written(
        &mut self,
        packet_id: u16,
        written: usize,
        len: usize,
    ) -> bool {
        if let Some(entry) = self
            .pending_release
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state.set_written(written, len);
            true
        } else {
            false
        }
    }

    pub(super) fn flush_release(&mut self, packet_id: u16) -> bool {
        if let Some(entry) = self
            .pending_release
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state = SendState::Sent;
            true
        } else {
            false
        }
    }

    pub(super) fn arm_replay(&mut self) {
        if !self.has_pending_state() {
            return;
        }

        trace!(
            "Arming outbound replay control={=usize} retained={=usize} pending_release={=usize} tx_used={=usize} tx_capacity={=usize}",
            self.pending_control.len(),
            self.retained.len(),
            self.pending_release.len(),
            self.retained_bytes(),
            self.buf.len()
        );
        self.mark_publish_dup();
        for entry in &mut self.pending_control {
            entry.state = SendState::Write { written: 0 };
        }
        for entry in &mut self.retained {
            entry.state = SendState::Write { written: 0 };
        }
        for entry in &mut self.pending_release {
            entry.state = SendState::Write { written: 0 };
        }
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

pub(super) fn serialize_control_packet<E>(
    buffer: &mut [u8],
    packet: ControlAction,
    maximum_packet_size: Option<u32>,
) -> Result<&[u8], Error<E>> {
    let bytes = encode_control_packet(buffer, packet)?;
    if maximum_packet_size.is_some_and(|max| bytes.len() > max as usize) {
        return Err(Error::Resource(ResourceError::PacketTooLarge));
    }
    Ok(bytes)
}

fn encode_control_packet(buffer: &mut [u8], packet: ControlAction) -> Result<&[u8], ProtocolError> {
    Ok(match packet {
        ControlAction::PubAck { packet_id, reason } => MqttSerializer::encode(
            buffer,
            &PubAck {
                packet_id,
                reason: reason.into(),
            },
        ),
        ControlAction::PubRec { packet_id, reason } => MqttSerializer::encode(
            buffer,
            &PubRec {
                packet_id,
                reason: reason.into(),
            },
        ),
        ControlAction::PubComp { packet_id, reason } => MqttSerializer::encode(
            buffer,
            &PubComp {
                packet_id,
                reason: reason.into(),
            },
        ),
        ControlAction::PingReq => MqttSerializer::encode(buffer, &PingReq),
    }?)
}

fn require_packet_size(maximum_packet_size: Option<u32>, len: usize) -> Result<(), ProtocolError> {
    if maximum_packet_size.is_some_and(|max| len > max as usize) {
        return Err(ProtocolError::PacketTooLarge);
    }
    Ok(())
}

pub(super) fn check_control_packet_size(
    maximum_packet_size: Option<u32>,
    action: ControlAction,
) -> Result<(), ProtocolError> {
    let mut buffer = [0u8; CONTROL_PACKET_WORKSPACE];
    let len = encode_control_packet(&mut buffer, action)?.len();
    require_packet_size(maximum_packet_size, len)
}

pub(super) fn check_pubrel_size(
    maximum_packet_size: Option<u32>,
    packet_id: u16,
    reason: ReasonCode,
) -> Result<(), ProtocolError> {
    let mut buffer = [0u8; CONTROL_PACKET_WORKSPACE];
    let len = encode_pubrel(&mut buffer, packet_id, reason)?.len();
    require_packet_size(maximum_packet_size, len)
}

pub(super) fn serialize_pubrel<E>(
    buffer: &mut [u8],
    packet_id: u16,
    reason: ReasonCode,
    maximum_packet_size: Option<u32>,
) -> Result<&[u8], Error<E>> {
    let bytes = encode_pubrel(buffer, packet_id, reason)?;
    if maximum_packet_size.is_some_and(|max| bytes.len() > max as usize) {
        return Err(Error::Resource(ResourceError::PacketTooLarge));
    }
    Ok(bytes)
}

fn encode_pubrel(
    buffer: &mut [u8],
    packet_id: u16,
    reason: ReasonCode,
) -> Result<&[u8], ProtocolError> {
    Ok(MqttSerializer::encode(
        buffer,
        &PubRel {
            packet_id,
            reason: reason.into(),
        },
    )?)
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
    use super::{ControlAction, MAX_FIXED_HEADER_SIZE, Outbound, OutboundStep, SendState};
    use crate::mqtt_client::OpKind;
    use crate::{
        Error, Properties, PubError, ReasonCode, ResourceError,
        packets::{PublishHeader, Subscribe},
        publication::Publication,
        types::TopicFilter,
        wire::Utf8String,
    };

    #[test]
    fn encoded_packet_is_packed_after_retained_prefix() {
        let mut storage = [0u8; 64];
        let mut outbound = Outbound::new(&mut storage);

        outbound.retain_packet(OpKind::Subscribe, 7, 10).unwrap();

        let len = outbound
            .encode_packet(&Subscribe {
                packet_id: 16,
                properties: Properties::from_slice(&[]),
                topics: &[TopicFilter::new("ABC")],
            })
            .unwrap();

        assert_eq!(&outbound.retained_packet(10, len)[..2], &[0x82, 0x09]);
    }

    #[test]
    fn can_retain_requires_fixed_header_scratch() {
        let mut storage = [0u8; MAX_FIXED_HEADER_SIZE + 4];
        let mut outbound = Outbound::new(&mut storage);

        outbound.retain_packet(OpKind::Subscribe, 7, 5).unwrap();

        assert!(!outbound.can_retain());
    }

    #[test]
    fn encode_publish_returns_insufficient_memory_when_only_header_gap_remains() {
        let mut storage = [0u8; MAX_FIXED_HEADER_SIZE + 4];
        let mut outbound = Outbound::new(&mut storage);

        outbound.retain_packet(OpKind::Subscribe, 7, 5).unwrap();

        let publication = Publication::bytes("a", b"x");
        let header = PublishHeader {
            topic: Utf8String(publication.topic),
            packet_id: None,
            properties: publication.properties,
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
        outbound.set_control_written(action, 5, 5);

        outbound.arm_replay();

        assert!(matches!(
            outbound.next_step(),
            Some(OutboundStep::Control(step))
                if step.action == action && step.state == SendState::Write { written: 0 }
        ));
    }

    #[test]
    fn acknowledgement_must_match_retained_operation() {
        let mut storage = [0u8; 16];
        let mut outbound = Outbound::new(&mut storage);
        outbound.retain_packet(OpKind::Subscribe, 7, 5).unwrap();

        assert!(!outbound.ack_packet(OpKind::PublishAtLeastOnce, 7));
        assert!(outbound.has_retained(7));
        assert!(outbound.ack_packet(OpKind::Subscribe, 7));
        assert!(!outbound.has_retained(7));
    }
}
