/// This module represents the session state of an MQTT communication session.
///
///
use embedded_nal::IpAddr;
use heapless::{consts, String, Vec};

pub struct SessionState {
    // Indicates that we are connected to a broker.
    pub connected: bool,
    pub keep_alive_interval: u16,
    pub broker: IpAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<consts::U64>,
    pub pending_subscriptions: Vec<u16, consts::U32>,
    packet_id: u16,
    active: bool,
}

impl SessionState {
    pub fn new<'a>(broker: IpAddr, id: String<consts::U64>) -> SessionState {
        SessionState {
            connected: false,
            active: false,
            broker,
            client_id: id,
            packet_id: 1,
            keep_alive_interval: 0,
            pending_subscriptions: Vec::new(),
            maximum_packet_size: None,
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.connected = false;
        self.packet_id = 1;
        self.keep_alive_interval = 0;
        self.maximum_packet_size = None;
        self.pending_subscriptions.clear();
    }

    /// Called whenever an active connection has been made with a broker.
    pub fn register_connection(&mut self) {
        self.active = true;
    }

    /// Indicates if there is present session state available.
    pub fn is_present(&self) -> bool {
        self.active
    }

    pub fn get_packet_identifier(&mut self) -> u16 {
        self.packet_id
    }

    pub fn increment_packet_identifier(&mut self) {
        let (result, overflow) = self.packet_id.overflowing_add(1);

        // Packet identifiers must always be non-zero.
        if overflow {
            self.packet_id = 1;
        } else {
            self.packet_id = result;
        }
    }
}
