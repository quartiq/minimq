/// This module represents the session state of an MQTT communication session.
use crate::warn;
use embedded_nal::IpAddr;
use heapless::{String, Vec};

use embedded_time::{
    duration::{Extensions, Seconds},
    Clock, Instant,
};

/// The default duration to wait for a ping response from the broker.
const PING_TIMEOUT: Seconds = Seconds(5);

pub struct SessionState<C: Clock> {
    pub keep_alive_interval: Option<Seconds<u32>>,
    ping_timeout: Option<Instant<C>>,
    next_ping: Option<Instant<C>>,
    pub broker: IpAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<64>,
    pub pending_subscriptions: Vec<u16, 32>,
    packet_id: u16,
    active: bool,
}

impl<C: Clock> SessionState<C> {
    pub fn new<'a>(broker: IpAddr, id: String<64>) -> SessionState<C> {
        SessionState {
            active: false,
            ping_timeout: None,
            next_ping: None,
            broker,
            client_id: id,
            packet_id: 1,
            keep_alive_interval: Some(59.seconds()),
            pending_subscriptions: Vec::new(),
            maximum_packet_size: None,
        }
    }

    pub fn reset(&mut self) {
        self.active = false;
        self.packet_id = 1;
        self.keep_alive_interval = Some(59.seconds());
        self.maximum_packet_size = None;
        self.pending_subscriptions.clear();
    }

    /// Called whenever an active connection has been made with a broker.
    pub fn register_connection(&mut self, now: Instant<C>) {
        self.active = true;
        self.ping_timeout = None;

        // If the keep-alive interval is specified, set up a ping to go out before the interval
        // elapses.
        if let Some(interval) = self.keep_alive_interval {
            self.next_ping.replace(now + interval - 500.milliseconds());
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
    pub fn handle_ping(&mut self, now: Instant<C>) -> Result<bool, ()> {
        // First, check if a ping is currently awaiting response.
        if let Some(timeout) = self.ping_timeout {
            if now > timeout {
                return Err(());
            }

            return Ok(false);
        }

        if let Some(ping_deadline) = self.next_ping {
            // Update the next ping deadline if the ping is due.
            if now > ping_deadline {
                self.next_ping
                    .replace(now + self.keep_alive_interval.unwrap() - 500.milliseconds());
                self.ping_timeout.replace(now + PING_TIMEOUT);
                return Ok(true);
            }
        }

        Ok(false)
    }
}
