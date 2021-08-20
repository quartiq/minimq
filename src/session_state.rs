/// This module represents the session state of an MQTT communication session.
///
///
use embedded_nal::IpAddr;
use heapless::{String, Vec};

use embedded_time::{
    duration::{Extensions, Seconds},
    Clock, Instant,
};

pub struct SessionState<C: Clock> {
    // Indicates that we are connected to a broker.
    pub connected: bool,
    pub keep_alive_interval: Option<Seconds<u32>>,
    pub ping_timeout: Option<Instant<C>>,
    pub broker: IpAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<64>,
    pub pending_subscriptions: Vec<u16, 32>,
    last_transmission: Option<Instant<C>>,
    last_reception: Option<Instant<C>>,
    packet_id: u16,
    active: bool,
}

impl<C: Clock> SessionState<C> {
    pub fn new<'a>(broker: IpAddr, id: String<64>) -> SessionState<C> {
        SessionState {
            connected: false,
            active: false,
            ping_timeout: None,
            broker,
            client_id: id,
            packet_id: 1,
            keep_alive_interval: Some(59.seconds()),
            last_transmission: None,
            last_reception: None,
            pending_subscriptions: Vec::new(),
            maximum_packet_size: None,
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.connected = false;
        self.packet_id = 1;
        self.keep_alive_interval = Some(59.seconds());
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

    pub fn handle_connection_reset(&mut self) {
        self.last_transmission = None;
        self.last_reception = None;
        self.ping_timeout = None;
    }

    pub fn register_transmission(&mut self, now: Instant<C>) {
        self.last_transmission = Some(now);
    }

    pub fn register_reception(&mut self, now: Instant<C>) {
        self.last_reception = Some(now);
    }

    pub fn ping_is_due(&self, now: &Instant<C>) -> bool {
        // Send a ping if we haven't sent or received data within 50% of the keep-alive interval.
        if let Some(interval) = self.keep_alive_interval {
            let tx_timeout = self
                .last_transmission
                .map_or(false, |last| *now > last + 500.milliseconds() * interval.0);

            let rx_timeout = self
                .last_reception
                .map_or(false, |last| *now > last + 500.milliseconds() * interval.0);

            tx_timeout || rx_timeout
        } else {
            false
        }
    }
}
