/// This module represents the session state of an MQTT communication session.
///
///
use embedded_nal::IpAddr;
use heapless::{consts, String, Vec};

pub struct SessionState {
    pub connected: bool,
    pub keep_alive_interval: u16,
    pub broker: IpAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<consts::U32>,
    pub pending_subscriptions: Vec<u16, consts::U32>,
    packet_id: u16,
}

impl SessionState {
    pub fn new(broker: IpAddr) -> SessionState {
        SessionState {
            connected: false,
            broker,
            client_id: String::new(),
            packet_id: 1,
            keep_alive_interval: 0,
            pending_subscriptions: Vec::new(),
            maximum_packet_size: None,
        }
    }

    pub fn reset(&mut self) {
        self.connected = false;
        self.packet_id = 1;
        self.keep_alive_interval = 0;
        self.maximum_packet_size = None;
        self.pending_subscriptions.clear();
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
