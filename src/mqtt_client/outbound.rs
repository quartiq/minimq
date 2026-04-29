use crate::packets::Pub;
use crate::packets::{PingReq, PubAck, PubComp, PubRec, PubRel};
use crate::ser::MAX_FIXED_HEADER_SIZE;
use crate::{Error, ProtocolError, PubError, ReasonCode, error, trace};
use heapless::Vec;

use super::Io;

const CONTROL_PACKET_LEN: usize = 9;
pub(super) const MAX_RETAINED: usize = 8;
pub(super) const MAX_PENDING_CONTROL: usize = 8;
pub(super) const MAX_PENDING_RELEASE: usize = 8;

#[derive(Debug, Copy, Clone, PartialEq)]
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
    packet_id: u16,
    offset: usize,
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
    used: usize,
    pending_control: Vec<PendingControl, MAX_PENDING_CONTROL>,
    retained: Vec<RetainedPacket, MAX_RETAINED>,
    pending_release: Vec<PendingRelease, MAX_PENDING_RELEASE>,
}

impl<'a> Outbound<'a> {
    pub(super) fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            used: 0,
            pending_control: Vec::new(),
            retained: Vec::new(),
            pending_release: Vec::new(),
        }
    }

    pub(super) fn clear(&mut self) {
        self.used = 0;
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

    pub(super) fn used(&self) -> usize {
        self.used
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

    pub(super) fn can_retain(&mut self) -> bool {
        self.compact();
        self.retained.len() < self.retained.capacity()
            && self.buf.len().saturating_sub(self.used) >= MAX_FIXED_HEADER_SIZE
    }

    pub(super) fn scratch_space(&mut self) -> &mut [u8] {
        self.compact();
        &mut self.buf[self.used..]
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

    pub(super) fn ack_packet(&mut self, packet_id: u16) -> bool {
        let Some(position) = self
            .retained
            .iter()
            .position(|entry| entry.packet_id == packet_id)
        else {
            return false;
        };
        self.retained.swap_remove(position);
        self.compact();
        true
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

    pub(super) fn mark_retained_dup(&mut self) {
        for entry in &self.retained {
            self.buf[entry.offset] |= 1 << 3;
        }
    }

    pub(super) fn encode_publish<P: crate::publication::ToPayload, E>(
        &mut self,
        packet: Pub<'_, P>,
    ) -> Result<(usize, usize), PubError<P::Error, E>> {
        self.compact();
        let start = self.used;
        let (offset, packet) =
            crate::ser::MqttSerializer::pub_to_buffer_meta(&mut self.buf[start..], packet)
                .map_err(|err| match err {
                    crate::ser::PubError::Encode(err) => {
                        PubError::Session(Error::Protocol(err.into()))
                    }
                    crate::ser::PubError::Payload(err) => PubError::Payload(err),
                })?;
        Ok((start + offset, packet.len()))
    }

    pub(super) fn encode_packet<T>(&mut self, packet: &T) -> Result<(usize, usize), ProtocolError>
    where
        T: serde::Serialize + crate::message_types::ControlPacket,
    {
        self.compact();
        let start = self.used;
        let (offset, packet) =
            crate::ser::MqttSerializer::to_buffer_meta(&mut self.buf[start..], packet)
                .map_err(ProtocolError::from)?;
        Ok((start + offset, packet.len()))
    }

    pub(super) fn retained_packet(&self, offset: usize, len: usize) -> &[u8] {
        &self.buf[offset..offset + len]
    }

    pub(super) fn retain_packet(
        &mut self,
        packet_id: u16,
        offset: usize,
        len: usize,
    ) -> Result<(), ProtocolError> {
        self.retained
            .push(RetainedPacket {
                packet_id,
                offset,
                len,
                state: SendState::Write { written: 0 },
            })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)?;
        self.used = self.used.max(offset + len);
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
            for entry in &self.retained {
                if entry.state.matches_priority(in_progress) {
                    return Some(OutboundStep::Retained(RetainedStep {
                        packet_id: entry.packet_id,
                        offset: entry.offset,
                        len: entry.len,
                        state: entry.state,
                    }));
                }
            }
        }
        None
    }

    pub(super) fn set_control_written(
        &mut self,
        action: ControlAction,
        written: usize,
        len: usize,
    ) {
        if let Some(entry) = self
            .pending_control
            .iter_mut()
            .find(|entry| entry.action == action)
        {
            entry.state.set_written(written, len);
        }
    }

    pub(super) fn flush_control(&mut self, action: ControlAction) {
        if let Some(entry) = self
            .pending_control
            .iter_mut()
            .find(|entry| entry.action == action)
        {
            entry.state = SendState::Sent;
        }
        self.pending_control
            .retain(|entry| entry.state != SendState::Sent);
    }

    pub(super) fn set_retained_written(&mut self, packet_id: u16, written: usize, len: usize) {
        if let Some(entry) = self
            .retained
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state.set_written(written, len);
        }
    }

    pub(super) fn flush_retained(&mut self, packet_id: u16) {
        if let Some(entry) = self
            .retained
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state = SendState::Sent;
        }
    }

    pub(super) fn set_release_written(&mut self, packet_id: u16, written: usize, len: usize) {
        if let Some(entry) = self
            .pending_release
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state.set_written(written, len);
        }
    }

    pub(super) fn flush_release(&mut self, packet_id: u16) {
        if let Some(entry) = self
            .pending_release
            .iter_mut()
            .find(|entry| entry.packet_id == packet_id)
        {
            entry.state = SendState::Sent;
        }
    }

    pub(super) fn arm_replay(&mut self) {
        if !self.has_pending_state() {
            return;
        }

        trace!(
            "Arming outbound replay control={} retained={} pending_release={} tx_used={} tx_capacity={}",
            self.pending_control.len(),
            self.retained.len(),
            self.pending_release.len(),
            self.used,
            self.buf.len()
        );
        self.mark_retained_dup();
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

    fn compact(&mut self) {
        let previous_used = self.used;
        self.retained.sort_unstable_by_key(|entry| entry.offset);

        let mut cursor = 0;
        let mut moved = 0;
        for entry in self.retained.iter_mut() {
            if entry.offset != cursor {
                self.buf
                    .copy_within(entry.offset..entry.offset + entry.len, cursor);
                entry.offset = cursor;
                moved += 1;
            }
            cursor += entry.len;
        }
        self.used = cursor;
        if moved != 0 || previous_used != self.used {
            trace!(
                "Compacted outbound buffer moved={} tx_used={} -> {} retained={} pending_release={}",
                moved,
                previous_used,
                self.used,
                self.retained.len(),
                self.pending_release.len()
            );
        }
    }
}

pub(super) fn serialize_control_packet<E>(
    buffer: &mut [u8],
    packet: ControlAction,
    maximum_packet_size: Option<u32>,
) -> Result<&[u8], Error<E>> {
    let bytes = encode_control_packet(buffer, packet).map_err(Error::Protocol)?;
    if maximum_packet_size.is_some_and(|max| bytes.len() > max as usize) {
        return Err(Error::Protocol(ProtocolError::Failed(
            ReasonCode::PacketTooLarge,
        )));
    }
    Ok(bytes)
}

fn encode_control_packet(buffer: &mut [u8], packet: ControlAction) -> Result<&[u8], ProtocolError> {
    match packet {
        ControlAction::PubAck { packet_id, reason } => crate::ser::MqttSerializer::to_buffer(
            buffer,
            &PubAck {
                packet_identifier: packet_id,
                reason: reason.into(),
            },
        ),
        ControlAction::PubRec { packet_id, reason } => crate::ser::MqttSerializer::to_buffer(
            buffer,
            &PubRec {
                packet_id,
                reason: reason.into(),
            },
        ),
        ControlAction::PubComp { packet_id, reason } => crate::ser::MqttSerializer::to_buffer(
            buffer,
            &PubComp {
                packet_id,
                reason: reason.into(),
            },
        ),
        ControlAction::PingReq => crate::ser::MqttSerializer::to_buffer(buffer, &PingReq),
    }
    .map_err(ProtocolError::from)
}

fn require_packet_size(maximum_packet_size: Option<u32>, len: usize) -> Result<(), ProtocolError> {
    if maximum_packet_size.is_some_and(|max| len > max as usize) {
        return Err(ProtocolError::Failed(ReasonCode::PacketTooLarge));
    }
    Ok(())
}

pub(super) fn check_control_packet_size(
    maximum_packet_size: Option<u32>,
    action: ControlAction,
) -> Result<(), ProtocolError> {
    let mut buffer = [0u8; CONTROL_PACKET_LEN];
    let len = encode_control_packet(&mut buffer, action)?.len();
    require_packet_size(maximum_packet_size, len)
}

pub(super) fn check_pubrel_size(
    maximum_packet_size: Option<u32>,
    packet_id: u16,
    reason: ReasonCode,
) -> Result<(), ProtocolError> {
    let mut buffer = [0u8; CONTROL_PACKET_LEN];
    let len = encode_pubrel(&mut buffer, packet_id, reason)?.len();
    require_packet_size(maximum_packet_size, len)
}

pub(super) fn serialize_pubrel<E>(
    buffer: &mut [u8],
    packet_id: u16,
    reason: ReasonCode,
    maximum_packet_size: Option<u32>,
) -> Result<&[u8], Error<E>> {
    let bytes = encode_pubrel(buffer, packet_id, reason).map_err(Error::Protocol)?;
    if maximum_packet_size.is_some_and(|max| bytes.len() > max as usize) {
        return Err(Error::Protocol(ProtocolError::Failed(
            ReasonCode::PacketTooLarge,
        )));
    }
    Ok(bytes)
}

fn encode_pubrel(
    buffer: &mut [u8],
    packet_id: u16,
    reason: ReasonCode,
) -> Result<&[u8], ProtocolError> {
    crate::ser::MqttSerializer::to_buffer(
        buffer,
        &PubRel {
            packet_id,
            reason: reason.into(),
        },
    )
    .map_err(ProtocolError::from)
}

pub(super) async fn write_packet<C: Io, T>(
    buffer: &mut [u8],
    connection: &mut C,
    packet: &T,
) -> Result<(), Error<C::Error>>
where
    T: serde::Serialize + crate::message_types::ControlPacket + core::fmt::Debug,
{
    let bytes = crate::ser::MqttSerializer::to_buffer(buffer, packet)
        .map_err(|err| Error::Protocol(err.into()))?;
    write_all(connection, bytes).await?;
    connection.flush().await.map_err(Error::Transport)?;
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
    use super::{ControlAction, Outbound, OutboundStep, SendState};
    use crate::{
        packets::Subscribe,
        publication::Publication,
        types::{Properties, TopicFilter},
    };

    #[test]
    fn encode_packet_returns_absolute_offset_after_retained_prefix() {
        let mut storage = [0u8; 64];
        let mut outbound = Outbound::new(&mut storage);

        outbound.retain_packet(7, 0, 10).unwrap();

        let (offset, len) = outbound
            .encode_packet(&Subscribe {
                packet_id: 16,
                dup: false,
                properties: Properties::Slice(&[]),
                topics: &[TopicFilter::new("ABC")],
            })
            .unwrap();

        assert_eq!(&outbound.retained_packet(offset, len)[..2], &[0x82, 0x09]);
    }

    #[test]
    fn can_retain_requires_fixed_header_scratch() {
        let mut storage = [0u8; super::MAX_FIXED_HEADER_SIZE + 4];
        let mut outbound = Outbound::new(&mut storage);

        outbound.retain_packet(7, 0, 5).unwrap();

        assert!(!outbound.can_retain());
    }

    #[test]
    fn encode_publish_returns_insufficient_memory_when_only_header_gap_remains() {
        let mut storage = [0u8; super::MAX_FIXED_HEADER_SIZE + 4];
        let mut outbound = Outbound::new(&mut storage);

        outbound.retain_packet(7, 0, 5).unwrap();

        let result = outbound
            .encode_publish::<_, ()>(crate::packets::Pub::from(Publication::bytes("a", b"x")));

        assert!(matches!(
            result,
            Err(crate::PubError::Session(crate::Error::Protocol(
                crate::ProtocolError::Encode(crate::SerError::InsufficientMemory)
            )))
        ));
    }

    #[test]
    fn arm_replay_restarts_pending_control_from_byte_zero() {
        let mut storage = [0u8; 32];
        let mut outbound = Outbound::new(&mut storage);
        let action = ControlAction::PubAck {
            packet_id: 7,
            reason: crate::ReasonCode::Success,
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
}
