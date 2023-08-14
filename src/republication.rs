use crate::packets::PubRel;
use crate::Error;
use crate::{
    network_manager::InterfaceHolder, reason_codes::ReasonCode, ring_buffer::RingBuffer,
    ProtocolError,
};
use core::convert::TryInto;
use heapless::Vec;

use embedded_nal::TcpClientStack;

pub(crate) struct RepublicationBuffer<'a> {
    publish_buffer: RingBuffer<'a>,
    pending_pubrel: Vec<(u16, ReasonCode), 10>,
    republish_index: Option<usize>,
    pubrel_republish_index: Option<usize>,
    max_tx_size: usize,
}

impl<'a> RepublicationBuffer<'a> {
    pub fn new(buf: &'a mut [u8], max_tx_size: usize) -> Self {
        Self {
            publish_buffer: RingBuffer::new(buf),
            pending_pubrel: Vec::new(),
            republish_index: None,
            pubrel_republish_index: None,
            max_tx_size,
        }
    }

    pub fn clear(&mut self) {
        self.republish_index.take();
        self.pubrel_republish_index.take();
        self.publish_buffer.clear();
        self.pending_pubrel.clear();
    }

    pub fn reset(&mut self) {
        self.republish_index.replace(0);
        self.pubrel_republish_index.replace(0);

        // If there's nothing to republish, clear out our states.
        if self.pending_pubrel.is_empty() {
            self.pubrel_republish_index.take();
        }

        if self.publish_buffer.len() == 0 {
            self.republish_index.take();
        }
    }

    pub fn pop_publish(&mut self, id: u16) -> Result<(), ProtocolError> {
        let (_, header) = self
            .publish_buffer
            .probe_header(0)
            .ok_or(ProtocolError::BadIdentifier)?;
        if header.packet_id != id {
            return Err(ProtocolError::BadIdentifier);
        }

        if let Some(index) = self.republish_index.take() {
            if index > 1 {
                self.republish_index.replace(index - 1);
            }
        }

        self.publish_buffer.pop(header.len);
        Ok(())
    }

    pub fn push_publish(&mut self, packet: &[u8]) -> Result<(), ProtocolError> {
        if self.publish_buffer.push_slice(packet).is_some() {
            return Err(ProtocolError::BufferSize);
        }

        Ok(())
    }

    pub fn pop_pubrel(&mut self, id: u16) -> Result<(), ProtocolError> {
        // We always have to pop from the front of the vector to enforce FIFO characteristics.
        let Some((pending_id, _)) = self.pending_pubrel.get(0) else {
            return Err(ProtocolError::UnexpectedPacket);
        };

        if *pending_id != id {
            return Err(ProtocolError::BadIdentifier);
        }

        // Now that we received the PubComp for this PubRel, we can remove it from our session
        // state. We will not need to retransmit this upon reconnection.
        self.pending_pubrel.remove(0);

        if let Some(index) = self.pubrel_republish_index.take() {
            if index > 1 {
                self.pubrel_republish_index.replace(index - 1);
            }
        }

        Ok(())
    }

    pub fn push_pubrel(&mut self, pubrel: &PubRel) -> Result<(), ProtocolError> {
        self.pending_pubrel
            .push((pubrel.packet_id, pubrel.reason.code()))
            .map_err(|_| ProtocolError::BufferSize)?;

        Ok(())
    }

    pub fn pending_transactions(&self) -> bool {
        // If we have publications or pubrels pending, there's message transactions
        // underway
        self.publish_buffer.len() > 0 || self.pending_pubrel.len() > 0
    }

    pub fn can_publish(&self) -> bool {
        self.publish_buffer.remainder() >= self.max_tx_size
    }

    pub fn next_republication<T: TcpClientStack>(
        &mut self,
        net: &mut InterfaceHolder<'_, T>,
    ) -> Result<bool, Error<T::Error>> {
        // Finish off any pending pubrels
        if let Some(index) = &self.pubrel_republish_index {
            let (packet_id, code) = self.pending_pubrel[*index];
            let pubrel = PubRel {
                packet_id,
                reason: code.into(),
            };

            net.send_packet(&pubrel)?;

            if index + 1 < self.pending_pubrel.len() {
                self.pubrel_republish_index.replace(index + 1);
            } else {
                self.pubrel_republish_index.take();
            }

            return Ok(true);
        }

        if let Some(index) = &self.republish_index {
            let (offset, header) = self.publish_buffer.probe_header(*index).unwrap();

            let (head, tail) = self.publish_buffer.slices(offset, header.len);

            net.write_multipart(head, tail)?;

            if offset + header.len < self.publish_buffer.len() {
                self.republish_index.replace(index + 1);
            } else {
                self.republish_index.take();
            }

            return Ok(true);
        }

        Ok(false)
    }

    pub fn is_republishing(&self) -> bool {
        self.republish_index.is_some() || self.pubrel_republish_index.is_some()
    }

    pub fn max_send_quota(&self) -> u16 {
        self.pending_pubrel
            .capacity()
            .try_into()
            .unwrap_or(u16::MAX)
    }
}
