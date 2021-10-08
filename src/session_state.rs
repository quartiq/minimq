/// This module represents the session state of an MQTT communication session.
use crate::{warn, QoS};
use embedded_nal::IpAddr;
use heapless::{LinearMap, String, Vec};

use embedded_time::{
    duration::{Extensions, Milliseconds, Seconds},
    Instant,
};

/// The default duration to wait for a ping response from the broker.
const PING_TIMEOUT: Seconds = Seconds(5);

pub struct SessionState<Clock: embedded_time::Clock, const MSG_SIZE: usize, const MSG_COUNT: usize>
{
    keep_alive_interval: Option<Milliseconds<u32>>,
    ping_timeout: Option<Instant<Clock>>,
    next_ping: Option<Instant<Clock>>,
    pub broker: IpAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<64>,
    pub pending_subscriptions: Vec<u16, 32>,
    pub pending_publish: LinearMap<u16, Vec<u8, MSG_SIZE>, MSG_COUNT>,
    pub pending_publish_ordering: Vec<u16, MSG_COUNT>,
    packet_id: u16,
    active: bool,
}

impl<Clock: embedded_time::Clock, const MSG_SIZE: usize, const MSG_COUNT: usize>
    SessionState<Clock, MSG_SIZE, MSG_COUNT>
{
    pub fn new(broker: IpAddr, id: String<64>) -> SessionState<Clock, MSG_SIZE, MSG_COUNT> {
        SessionState {
            active: false,
            ping_timeout: None,
            next_ping: None,
            broker,
            client_id: id,
            packet_id: 1,
            keep_alive_interval: Some(59_000.milliseconds()),
            pending_subscriptions: Vec::new(),
            pending_publish: LinearMap::new(),
            pending_publish_ordering: Vec::new(),
            maximum_packet_size: None,
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.packet_id = 1;
        self.keep_alive_interval = Some(59_000.milliseconds());
        self.maximum_packet_size = None;
        self.pending_subscriptions.clear();
        self.pending_publish.clear();
        self.pending_publish_ordering.clear();
    }

    /// Get the keep-alive interval as an integer number of seconds.
    ///
    /// # Note
    /// If no keep-alive interval is specified, zero is returned.
    pub fn keepalive_interval(&self) -> u16 {
        (self.keep_alive_interval.unwrap_or(0.milliseconds()).0 * 1000) as u16
    }

    /// Update the keep-alive interval.
    ///
    /// # Args
    /// * `seconds` - The number of seconds in the keep-alive interval.
    pub fn set_keepalive(&mut self, seconds: u16) {
        self.keep_alive_interval
            .replace(Milliseconds(seconds as u32 * 1000));
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
            QoS::AtLeastOnce => self.pending_publish.len() < MSG_COUNT,
            QoS::ExactlyOnce => false,
        }
    }

    pub fn pending_messages(&self, qos: QoS) -> usize {
        match qos {
            QoS::AtMostOnce => 0,
            QoS::AtLeastOnce => self.pending_publish.len(),
            QoS::ExactlyOnce => 0,
        }
    }

    /// Called whenever an active connection has been made with a broker.
    pub fn register_connection(&mut self, now: Instant<Clock>) {
        self.active = true;
        self.ping_timeout = None;

        // The next ping should be sent out in half the keep-alive interval from now.
        if let Some(interval) = self.keep_alive_interval {
            self.next_ping.replace(now + interval / 2);
        }
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

    /// Callback function to register a PingResp packet reception.
    pub fn register_ping_response(&mut self) {
        // Take the current timeout to remove it.
        let timeout = self.ping_timeout.take();

        // If there was no timeout to begin with, log the spurious ping response.
        if timeout.is_none() {
            warn!("Got unexpected ping response");
        }
    }

    /// Handle ping time management.
    ///
    /// # Args
    /// * `now` - The current instant in time.
    ///
    /// # Returns
    /// An error if a pending ping is past the deadline. Otherwise, returns a bool indicating
    /// whether or not a ping should be sent.
    pub fn handle_ping(&mut self, now: Instant<Clock>) -> Result<bool, ()> {
        // First, check if a ping is currently awaiting response.
        if let Some(timeout) = self.ping_timeout {
            if now > timeout {
                return Err(());
            }

            return Ok(false);
        }

        if let Some((keep_alive_interval, ping_deadline)) =
            self.keep_alive_interval.zip(self.next_ping)
        {
            // Update the next ping deadline if the ping is due.
            if now > ping_deadline {
                // The next ping should be sent out in half the keep-alive interval from now.
                self.next_ping.replace(now + keep_alive_interval / 2);

                self.ping_timeout.replace(now + PING_TIMEOUT);
                return Ok(true);
            }
        }

        Ok(false)
    }
}
