use crate::packets::Pub;
use crate::{Error, ProtocolError, PubError, ReasonCode};
use embedded_io_async::Error as _;
use heapless::Vec;

use super::Io;

const CONTROL_PACKET_LEN: usize = 9;
pub(super) const MAX_RETAINED: usize = 4;
pub(super) const MAX_PENDING_RELEASE: usize = 4;

#[derive(Debug, Copy, Clone, PartialEq)]
pub(super) struct PendingRelease {
    pub(super) packet_id: u16,
    pub(super) reason: ReasonCode,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
struct RetainedPacket {
    packet_id: u16,
    offset: usize,
    len: usize,
}

#[derive(Debug)]
pub(super) struct Outbound<'a> {
    buf: &'a mut [u8],
    used: usize,
    retained: Vec<RetainedPacket, MAX_RETAINED>,
    pending_release: Vec<PendingRelease, MAX_PENDING_RELEASE>,
    replay_armed: bool,
}

impl<'a> Outbound<'a> {
    pub(super) fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            used: 0,
            retained: Vec::new(),
            pending_release: Vec::new(),
            replay_armed: false,
        }
    }

    pub(super) fn clear(&mut self) {
        self.used = 0;
        self.retained.clear();
        self.pending_release.clear();
        self.replay_armed = false;
    }

    fn has_pending_state(&self) -> bool {
        !self.retained.is_empty() || !self.pending_release.is_empty()
    }

    pub(super) fn retained_full(&self) -> bool {
        self.retained.is_full()
    }

    pub(super) fn retained_packets(&self) -> impl Iterator<Item = &[u8]> {
        self.retained
            .iter()
            .map(|entry| &self.buf[entry.offset..entry.offset + entry.len])
    }

    pub(super) fn max_inflight(&self) -> u16 {
        MAX_RETAINED.min(MAX_PENDING_RELEASE) as u16
    }

    pub(super) fn can_retain(&mut self) -> bool {
        self.compact();
        self.retained.len() < self.retained.capacity() && self.used < self.buf.len()
    }

    pub(super) fn scratch_space(&mut self) -> &mut [u8] {
        self.compact();
        &mut self.buf[self.used..]
    }

    pub(super) fn ack_packet(&mut self, packet_id: u16) -> Result<(), ProtocolError> {
        let position = self
            .retained
            .iter()
            .position(|entry| entry.packet_id == packet_id)
            .ok_or(ProtocolError::BadIdentifier)?;
        self.retained.swap_remove(position);
        self.compact();
        Ok(())
    }

    pub(super) fn queue_release(
        &mut self,
        packet_id: u16,
        reason: ReasonCode,
    ) -> Result<(), ProtocolError> {
        self.pending_release
            .push(PendingRelease { packet_id, reason })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)
    }

    pub(super) fn ack_release(&mut self, packet_id: u16) -> Result<(), ProtocolError> {
        let position = self
            .pending_release
            .iter()
            .position(|pending| pending.packet_id == packet_id)
            .ok_or(ProtocolError::BadIdentifier)?;
        self.pending_release.swap_remove(position);
        Ok(())
    }

    pub(super) fn mark_retained_dup(&mut self) {
        for entry in &self.retained {
            self.buf[entry.offset] |= 1 << 3;
        }
    }

    pub(super) fn encode_publish<P: crate::publication::ToPayload>(
        &mut self,
        packet: Pub<'_, P>,
    ) -> Result<(usize, usize), PubError<P::Error>> {
        self.compact();
        let start = self.used;
        let (offset, packet) =
            crate::ser::MqttSerializer::pub_to_buffer_meta(&mut self.buf[start..], packet)
                .map_err(|err| match err {
                    crate::ser::PubError::Error(err) => {
                        PubError::Error(Error::Protocol(err.into()))
                    }
                    crate::ser::PubError::Other(err) => PubError::Serialization(err),
                })?;
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
            })
            .map_err(|_| ProtocolError::InflightMetadataExhausted)?;
        self.used = self.used.max(offset + len);
        Ok(())
    }

    pub(super) fn pending_releases(&self) -> impl Iterator<Item = PendingRelease> + '_ {
        self.pending_release.iter().copied()
    }

    pub(super) fn arm_replay(&mut self) {
        self.replay_armed = self.has_pending_state();
    }

    pub(super) fn needs_replay(&self) -> bool {
        self.replay_armed
    }

    pub(super) fn finish_replay(&mut self) {
        self.replay_armed = false;
    }

    fn compact(&mut self) {
        self.retained.sort_unstable_by_key(|entry| entry.offset);

        let mut cursor = 0;
        for entry in self.retained.iter_mut() {
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

pub(super) async fn write_packet<C: Io, T>(
    buffer: &mut [u8],
    connection: &mut C,
    packet: &T,
) -> Result<(), Error>
where
    T: serde::Serialize + crate::message_types::ControlPacket + core::fmt::Debug,
{
    let bytes = crate::ser::MqttSerializer::to_buffer(buffer, packet)
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

pub(super) async fn write_control_packet<C: Io, T>(
    connection: &mut C,
    packet: &T,
    maximum_packet_size: Option<u32>,
) -> Result<(), Error>
where
    T: serde::Serialize + crate::message_types::ControlPacket + core::fmt::Debug,
{
    let mut buffer = [0u8; CONTROL_PACKET_LEN];
    let bytes = crate::ser::MqttSerializer::to_buffer(&mut buffer, packet)
        .map_err(|err| Error::Protocol(err.into()))?;
    if maximum_packet_size.is_some_and(|max| bytes.len() > max as usize) {
        return Err(Error::Protocol(ProtocolError::Failed(
            ReasonCode::PacketTooLarge,
        )));
    }
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
