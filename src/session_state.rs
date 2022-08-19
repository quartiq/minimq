/// This module represents the session state of an MQTT communication session.
use crate::{warn, Error, ProtocolError, QoS};
use core::marker::PhantomData;
use embedded_nal::SocketAddr;
use embedded_nal::TcpClientStack;
use heapless::{LinearMap, String, Vec};

use embedded_time::{
    duration::{Extensions, Milliseconds, Seconds},
    Instant,
};

/// The default duration to wait for a ping response from the broker.
const PING_TIMEOUT: Seconds = Seconds(5);

pub struct MessageRecord<const N: usize> {
    pub msg: Vec<u8, N>,
    transmitted: bool,
    acknowledged: bool,
    _id: u16,
    qos: QoS,
}

pub struct SessionState<
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
    const MSG_SIZE: usize,
    const MSG_COUNT: usize,
> {
    keep_alive_interval: Option<Milliseconds<u32>>,
    ping_timeout: Option<Instant<Clock>>,
    next_ping: Option<Instant<Clock>>,
    pub broker: SocketAddr,
    pub maximum_packet_size: Option<u32>,
    pub client_id: String<64>,
    pub pending_subscriptions: Vec<u16, 32>,
    pending_publish: LinearMap<u16, MessageRecord<MSG_SIZE>, MSG_COUNT>,
    pending_publish_ordering: Vec<u16, MSG_COUNT>,
    packet_id: u16,
    active: bool,
    _stack: PhantomData<TcpStack>,
}

impl<
        TcpStack: TcpClientStack,
        Clock: embedded_time::Clock,
        const MSG_SIZE: usize,
        const MSG_COUNT: usize,
    > SessionState<TcpStack, Clock, MSG_SIZE, MSG_COUNT>
{
    pub fn new(
        broker: SocketAddr,
        id: String<64>,
    ) -> SessionState<TcpStack, Clock, MSG_SIZE, MSG_COUNT> {
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
            _stack: PhantomData::default(),
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
        (self
            .keep_alive_interval
            .unwrap_or_else(|| 0.milliseconds())
            .0
            / 1000) as u16
    }

    /// Update the keep-alive interval.
    ///
    /// # Args
    /// * `seconds` - The number of seconds in the keep-alive interval.
    pub fn set_keepalive(&mut self, seconds: u16) {
        self.keep_alive_interval
            .replace(Milliseconds(seconds as u32 * 1000));
    }

    /// Called when publish with QoS > 0 is called so that we can keep track of acknowledgement.
    pub fn handle_publish(
        &mut self,
        qos: QoS,
        id: u16,
        packet: &[u8],
    ) -> Result<(), Error<TcpStack::Error>> {
        let mut buf: Vec<u8, MSG_SIZE> = Vec::from_slice(packet).unwrap();
        // Set DUP = 1 (bit 3). If this packet is ever read it's just because we want to resend it
        buf[0] |= 1 << 3;

        let record = MessageRecord {
            _id: id,
            qos,
            msg: buf,
            transmitted: true,
            acknowledged: false,
        };

        self.pending_publish
            .insert(id, record)
            .map_err(|_| Error::Unsupported)?;
        self.pending_publish_ordering
            .push(id)
            .map_err(|_| Error::Unsupported)?;

        Ok(())
    }

    fn remove_packet(&mut self, id: u16) -> Result<(), Error<TcpStack::Error>> {
        // Remove the ID from our publication tracking. Note that we intentionally remove from both
        // the ordering and state management without checking success to ensure that state remains
        // valid.
        let item = self.pending_publish.remove(&id);
        let ordering = self
            .pending_publish_ordering
            .iter()
            .position(|&i| i == id)
            .map(|index| Some(self.pending_publish_ordering.remove(index)));

        // If the ID didn't exist in our state tracking, indicate the error to the user.
        if item.is_none() || ordering.is_none() {
            return Err(Error::Protocol(ProtocolError::BadIdentifier));
        }

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

    pub fn handle_pubrec(&mut self, id: u16) -> Result<(), Error<TcpStack::Error>> {
        let item = self
            .pending_publish
            .get_mut(&id)
            .ok_or_else(|| Error::Protocol(ProtocolError::BadIdentifier))?;

        if item.qos != QoS::ExactlyOnce {
            return Err(Error::Protocol(ProtocolError::WrongQos));
        }

        item.acknowledged = true;

        Ok(())
    }

    pub fn handle_pubcomp(&mut self, id: u16) -> Result<(), Error<TcpStack::Error>> {
        let item = self
            .pending_publish
            .get(&id)
            .ok_or_else(|| Error::Protocol(ProtocolError::BadIdentifier))?;

        if item.qos != QoS::ExactlyOnce {
            return Err(Error::Protocol(ProtocolError::WrongQos));
        }

        if !item.acknowledged {
            return Err(Error::Protocol(ProtocolError::BadIdentifier));
        }

        self.remove_packet(id)?;
        Ok(())
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

        // We just reconnected to the broker. Any of our pending publications will need to be
        // republished.
        for (_key, value) in self.pending_publish.iter_mut() {
            value.transmitted = false;
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
            !item.transmitted && !item.acknowledged
        });

        if let Some(key) = next_key {
            let mut item = self.pending_publish.get_mut(key).unwrap();
            item.transmitted = true;
            Some(&item.msg)
        } else {
            None
        }
    }
}
