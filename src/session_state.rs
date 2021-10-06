/// This module represents the session state of an MQTT communication session.
///
///
use crate::QoS;
use embedded_nal::IpAddr;
use heapless::{String, Vec, LinearMap};

use embedded_time::{
    duration::{Extensions, Seconds},
    Instant,
};

pub struct SessionState<Clock: embedded_time::Clock, const MSG_SIZE: usize, const MSG_COUNT: usize>
{
    // Indicates that we are connected to a broker.
    pub connected: bool,
    pub keep_alive_interval: Option<Seconds<u32>>,
    pub ping_timeout: Option<Instant<Clock>>,
    pub broker: IpAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<64>,
    last_transmission: Option<Instant<Clock>>,
    pub pending_subscriptions: Vec<u16, 32>,
    pub pending_publish: LinearMap<u16, Vec<u8, MSG_SIZE>, MSG_COUNT>,
    pub pending_publish_ordering: Vec<u16, MSG_COUNT>,
    packet_id: u16,
    active: bool,
}

impl<Clock: embedded_time::Clock, const MSG_SIZE: usize, const MSG_COUNT: usize> SessionState<Clock, MSG_SIZE, MSG_COUNT> {
    pub fn new(broker: IpAddr, id: String<64>) -> SessionState<Clock, MSG_SIZE, MSG_COUNT> {
        SessionState {
            connected: false,
            active: false,
            ping_timeout: None,
            broker,
            client_id: id,
            packet_id: 1,
            keep_alive_interval: Some(59.seconds()),
            last_transmission: None,
            pending_subscriptions: Vec::new(),
            pending_publish: LinearMap::new(),
            pending_publish_ordering: Vec::new(),
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
        self.pending_publish.clear();
        self.pending_publish_ordering.clear();
        self.last_transmission = None;
    }

    /// Called when publish with QoS 1 is called so that we can keep track of PUBACK
    pub fn handle_publish(&mut self, qos: QoS, id: u16, packet: &[u8]) {
        // This is not called for QoS 0 and QoS 2 is not implemented yet
        assert_eq!(qos, QoS::AtLeastOnce);

        let mut buf: Vec<u8, MSG_SIZE> = Vec::from_slice(packet).unwrap();
        // Set DUP = 1 (bit 3). If this packet is ever read it's just because we want to resend it
        buf[0] |= 1 << 3;
        // If this fails and the PUBACK will be received and Client (minimq) should disconnect from server with 0x82 Protocol Error
        // This behaviour pretty much reverts this message to QoS 0 with a restart if the message is actually delivered
        let _ = self.pending_publish.insert(id, buf);
        let _ = self.pending_publish_ordering.push(id);
    }

    /// Delete given pending publish as the server took ownership of it
    pub fn handle_puback(&mut self, id: u16) {
        self.pending_publish.remove(&id);
        let mut found = false;
        for i in 0..self.pending_publish_ordering.len() {
            if found {
                self.pending_publish_ordering[i - 1] = self.pending_publish_ordering[i];
            } else {
                if self.pending_publish_ordering[i] == id {
                    found = true;
                }
            }
        }
        self.pending_publish_ordering.pop();
    }

    /// Indicates if publish with QoS 1 is possible.
    pub fn can_publish(&self, qos: QoS) -> bool {
        match qos {
            QoS::AtMostOnce => true,
            QoS::AtLeastOnce => self.pending_publish.len() < MSG_SIZE,
            QoS::ExactlyOnce => false,
        }
    }

    pub fn pending_messages(&self, qos: QoS) -> usize {
        match qos {
            QoS::AtMostOnce => 0,
            QoS::AtLeastOnce => self.pending_publish.len(),
            QoS::ExactlyOnce => 0
        }
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

    pub fn register_transmission(&mut self, now: Instant<Clock>) {
        self.last_transmission = Some(now);
    }

    pub fn ping_is_due(&self, now: &Instant<Clock>) -> bool {
        // Send a ping if we haven't sent a transmission in the last 50% of the keepalive internal.
        self.keep_alive_interval
            .zip(self.last_transmission)
            .map_or(false, |(interval, last)| {
                *now > last + 500.milliseconds() * interval.0
            })
    }
}
