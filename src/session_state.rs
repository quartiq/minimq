use crate::packets::PubRel;
/// This module represents the session state of an MQTT communication session.
use crate::{
    network_manager::InterfaceHolder, reason_codes::ReasonCode, republication::RepublicationBuffer,
    Error, ProtocolError, QoS,
};
use embedded_nal::TcpClientStack;
use heapless::{String, Vec};

pub(crate) struct SessionState<'a> {
    pub(crate) client_id: String<64>,
    pub(crate) repub: RepublicationBuffer<'a>,

    /// Represents a list of packet_ids current in use by the server for Publish packets with
    /// QoS::ExactlyOnce
    pub pending_server_packet_ids: Vec<u16, 10>,

    packet_id: u16,
    active: bool,
    was_reset: bool,
}

impl<'a> SessionState<'a> {
    pub fn new(id: String<64>, buffer: &'a mut [u8], max_tx_size: usize) -> SessionState<'a> {
        SessionState {
            active: false,
            client_id: id,
            packet_id: 1,
            repub: RepublicationBuffer::new(buffer, max_tx_size),
            pending_server_packet_ids: Vec::new(),
            was_reset: false,
        }
    }

    pub fn register_connected(&mut self) {
        self.active = true;

        // Reset the republish indices
        self.repub.reset();
    }

    pub fn reset(&mut self) {
        // Only register a reset if we previously had an active session state.
        self.was_reset = self.active;
        self.active = false;
        self.packet_id = 1;
        self.repub.clear();
        self.pending_server_packet_ids.clear();
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
        packet: &[u8],
    ) -> Result<(), ProtocolError> {
        // QoS::AtMostOnce requires no additional state tracking.
        if qos == QoS::AtMostOnce {
            return Ok(());
        }

        self.repub.push_publish(id, packet)?;

        Ok(())
    }

    pub fn remove_packet(&mut self, packet_id: u16) -> Result<(), ProtocolError> {
        self.repub.pop_publish(packet_id)
    }

    pub fn handle_pubrec(&mut self, pubrel: &PubRel) -> Result<(), ProtocolError> {
        self.repub.push_pubrel(pubrel)
    }

    pub fn handle_pubcomp(&mut self, id: u16) -> Result<(), ProtocolError> {
        self.repub.pop_pubrel(id)
    }

    /// Indicates if publish with QoS 1 is possible.
    pub fn can_publish(&self, qos: QoS) -> bool {
        match qos {
            QoS::AtMostOnce => true,
            QoS::AtLeastOnce | QoS::ExactlyOnce => self.repub.can_publish(),
        }
    }

    pub fn handshakes_pending(&self) -> bool {
        // If we're exchanging messages with the server still, there's messages pending.
        if !self.pending_server_packet_ids.is_empty() {
            return false;
        }

        // Otherwise, there's handshakes pending if our republication state has something pending.
        self.repub.pending_transactions()
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
    pub fn next_pending_republication<T: TcpClientStack>(
        &mut self,
        net: &mut InterfaceHolder<'_, T>,
    ) -> Result<bool, Error<T::Error>> {
        self.repub.next_republication(net)
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

    /// The number of packets that we support receiving simultanenously.
    pub fn receive_maximum(&self) -> usize {
        self.pending_server_packet_ids.capacity()
    }

    pub fn max_send_quota(&self) -> u16 {
        self.repub.max_send_quota()
    }
}
