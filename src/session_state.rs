use heapless::{String, consts};
pub use embedded_nal::{IpAddr};

pub struct SessionState {
    pub connected: bool,
    pub session_expiry_interval: u32,
    pub keep_alive_interval: u16,
    pub broker: IpAddr,
    pub client_id: String<consts::U32>,
    packet_id: u16,
}

impl SessionState {
    pub fn new<'a>(broker: IpAddr, id: &'a str) -> SessionState {
        SessionState {
            connected: false,
            session_expiry_interval: 0,
            broker,
            client_id: String::from(id),
            packet_id: 1,
            keep_alive_interval: 0,
        }
    }

    pub fn reset(&mut self) {
        self.connected = false;
        self.packet_id = 1;
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

