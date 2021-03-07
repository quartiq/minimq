/// This module represents the session state of an MQTT communication session.
///
///
use embedded_nal::IpAddr;
use embedded_time::{duration::Seconds, Clock, Instant};
use heapless::{consts, String, Vec};

pub struct SessionState<C>
where
    C: Clock,
{
    pub connected: bool,

    /// (embedded_time doesn't support u16.)
    pub keep_alive_interval: Seconds<u32>,

    /// Timestamp of the last message sent to the broker, for keep-alive generation.
    pub last_write_time: Option<Instant<C>>,

    /// If we sent a ping request without having received a response, stores the time
    /// the request was sent.
    pub pending_pingreq_time: Option<Instant<C>>,

    pub broker: IpAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<consts::U64>,
    pub pending_subscriptions: Vec<u16, consts::U32>,
    packet_id: u16,
}

impl<C> SessionState<C>
where
    C: Clock,
{
    pub fn new<'a>(broker: IpAddr, id: String<consts::U64>) -> SessionState<C> {
        SessionState {
            connected: false,
            broker,
            client_id: id,
            packet_id: 1,
            keep_alive_interval: Seconds(10),
            last_write_time: None,
            pending_pingreq_time: None,
            pending_subscriptions: Vec::new(),
            maximum_packet_size: None,
        }
    }

    pub fn reset(&mut self) {
        self.connected = false;
        self.packet_id = 1;
        self.keep_alive_interval = Seconds(10);
        self.last_write_time = None;
        self.pending_pingreq_time = None;
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
