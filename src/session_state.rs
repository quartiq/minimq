/// This module represents the session state of an MQTT communication session.
use crate::{reason_codes::ReasonCode, ring_buffer::RingBuffer, Error, ProtocolError, QoS};
use core::marker::PhantomData;
use embedded_nal::TcpClientStack;
use heapless::{String, Vec};

pub struct SessionState<'a, TcpStack: TcpClientStack> {
    pub client_id: String<64>,
    pending_publications: RingBuffer<'a>,

    /// Represents a list of packet_ids current in use by the server for Publish packets with
    /// QoS::ExactlyOnce
    pub pending_server_packet_ids: Vec<u16, 10>,
    pub pending_client_pubrel: Vec<u16, 10>,

    packet_id: u16,
    active: bool,
    was_reset: bool,
    _stack: PhantomData<TcpStack>,
}

impl<'a, TcpStack: TcpClientStack> SessionState<'a, TcpStack> {
    pub fn new(id: String<64>, buffer: &'a mut [u8]) -> SessionState<'a, TcpStack> {
        SessionState {
            active: false,
            client_id: id,
            packet_id: 1,
            pending_publications: RingBuffer::new(buffer),
            pending_client_pubrel: Vec::new(),
            pending_server_packet_ids: Vec::new(),
            was_reset: false,
            _stack: PhantomData::default(),
        }
    }

    pub fn reset(&mut self) {
        // Only register a reset if we previously had an active session state.
        self.was_reset = self.active;
        self.active = false;
        self.packet_id = 1;
        self.pending_publications.reset();
        self.pending_client_pubrel.clear();
        self.pending_server_pubrel.clear();
    }

    /// Check if the session state has been reset.
    pub fn was_reset(&mut self) -> bool {
        let reset = self.was_reset;
        self.was_reset = false;
        reset
    }

    /// Called when publish with QoS > 0 is called so that we can keep track of acknowledgement.
    pub fn handle_publish(
        &mut self,
        qos: QoS,
        id: u16,
        packet: &mut [u8],
    ) -> Result<(), Error<TcpStack::Error>> {
        // QoS::AtMostOnce requires no additional state tracking.
        if qos == QoS::AtMostOnce {
            return Ok(());
        }

        // Set DUP = 1 (bit 3). If this packet is ever read it's just because we want to resend it
        packet[0] |= 1 << 3;

        self.pending_publish
            .push_slice(packet)
            .map_err(|_| Error::BufferSize)?;

        Ok(())
    }

    pub fn remove_packet(&mut self, id: u16) -> Result<(), ProtocolError> {
        let header = self.pending_publish.probe_header()?;
        if header.packet_id != id {
            return Err(ProtocolError::BadIdentifier);
        }

        // TODO: Length should be fully packet length, including all headers.
        self.pending_publish.pop(header.len)?;
        Ok(())
    }

    /// Delete given pending publish as the server took ownership of it
    pub fn handle_puback(&mut self, id: u16) -> Result<(), Error<TcpStack::Error>> {
        if let Some(item) = self.pending_publish.get(&id) {
            if item.qos != QoS::AtLeastOnce {
                return Err(Error::Protocol(ProtocolError::WrongQos));
            }
        }

        self.remove_packet(id)?;
        Ok(())
    }

    pub fn handle_pubrec(
        &mut self,
        packet_id: u16,
        pubrel: &[u8],
    ) -> Result<(), Error<TcpStack::Error>> {
        self.remove_packet(packet_id)?;
        self.pending_client_pubrel
            .push(pubrel)
            .map_err(|_| Error::BufferSize)?;

        Ok(())
    }

    pub fn handle_pubcomp(&mut self, id: u16) -> Result<(), Error<TcpStack::Error>> {
        let position = self
            .pending_client_pubrel
            .iter()
            .position(|packet_id| id == packet_id)
            .or_else(|| ProtocolError::Unackwnoledged.into())?;
        self.pending_client_pubrel.remove(position);

        Ok(())
    }

    /// Indicates if publish with QoS 1 is possible.
    pub fn can_publish(&self, qos: QoS) -> bool {
        match qos {
            QoS::AtMostOnce => true,

            // TODO: Should only publish if we have remaining space in our buffers
            QoS::AtLeastOnce | QoS::ExactlyOnce => self.pending_publish.len() < MSG_COUNT,
        }
    }

    pub fn pending_client_publications(&self, qos: QoS) -> usize {
        match qos {
            QoS::AtMostOnce => 0,
            _ => self
                .pending_publish
                .values()
                .filter(|&item| item.qos == qos)
                .count(),
        }
    }

    /// Indicates if there is present session state available.
    pub fn is_present(&self) -> bool {
        self.active
    }

    pub fn get_packet_identifier(&mut self) -> u16 {
        let packet_id = self.packet_id;

        let (result, overflow) = self.packet_id.overflowing_add(1);

        // Packet identifiers must always be non-zero.
        if overflow {
            self.packet_id = 1;
        } else {
            self.packet_id = result;
        }

        packet_id
    }

    /// Get the next message that needs to be republished.
    ///
    /// # Note
    /// Messages provided by this function must be properly transmitted to maintain proper state.
    ///
    /// # Returns
    /// The next message in the sequence that must be republished.
    pub fn next_pending_republication(&mut self) -> Option<&Vec<u8, MSG_SIZE>> {
        let next_key = self.pending_publish_ordering.iter().find(|&id| {
            let item = self.pending_publish.get(id).unwrap();
            !item.transmitted
        });

        if let Some(key) = next_key {
            let mut item = self.pending_publish.get_mut(key).unwrap();
            item.transmitted = true;
            Some(&item.msg)
        } else {
            None
        }
    }

    /// Check if a server packet ID is in use.
    pub fn server_packet_id_in_use(&self, id: u16) -> bool {
        self.pending_server_packet_ids
            .iter()
            .any(|&inflight_id| inflight_id == id)
    }

    /// Register a server packet ID as being in use.
    pub fn push_server_packet_id(&mut self, id: u16) -> ReasonCode {
        if self.pending_server_packet_ids.push(id).is_err() {
            ReasonCode::ReceiveMaxExceeded
        } else {
            ReasonCode::Success
        }
    }

    /// Mark a server packet ID as no longer in use.
    pub fn remove_server_packet_id(&mut self, id: u16) -> ReasonCode {
        if let Some(position) = self
            .pending_server_packet_ids
            .iter()
            .position(|&inflight_id| inflight_id == id)
        {
            self.pending_server_packet_ids.swap_remove(position);
            ReasonCode::Success
        } else {
            ReasonCode::PacketIdNotFound
        }
    }

    /// Get a list of all inflight server packet IDs.
    pub fn server_packet_ids(&self) -> &[u16] {
        &self.pending_server_packet_ids
    }
}
