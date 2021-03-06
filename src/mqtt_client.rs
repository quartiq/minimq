use crate::{
    de::{deserialize::ReceivedPacket, PacketReader},
    ser::serialize,
    session_state::SessionState,
    Property, {debug, error, info},
};

use core::{cell::RefCell, convert::TryFrom, str::FromStr};

use embedded_nal::{nb, IpAddr, Mode, SocketAddr};
use embedded_time::{duration::Seconds, fixed_point::FixedPoint, Clock};

use generic_array::{ArrayLength, GenericArray};
use heapless::String;

/// A client for interacting with an MQTT Broker.
pub struct MqttClient<T, N, C>
where
    T: ArrayLength<u8>,
    N: embedded_nal::TcpStack,
    C: Clock,
    u32: TryFrom<C::T>,
{
    /// The network stack originally provided to the client.
    pub network_stack: N,
    pub clock: C,

    socket: RefCell<Option<N::TcpSocket>>,
    state: RefCell<SessionState<C>>,
    packet_reader: PacketReader<T>,
    transmit_buffer: RefCell<GenericArray<u8, T>>,
    connect_sent: bool,
    ping_response_timeout: Seconds<u32>,
}

/// The quality-of-service for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum QoS {
    /// A packet will be delivered at most once, but may not be delivered at all.
    AtMostOnce,

    /// A packet will be delivered at least one time, but possibly more than once.
    AtLeastOnce,

    /// A packet will be delivered exactly one time.
    ExactlyOne,
}

/// Possible errors encountered during an MQTT connection.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Error<E> {
    Network(E),
    WriteFail,
    NotReady,
    Disconnected,
    Unsupported,
    ProvidedClientIdTooLong,
    Failed(u8),
    Protocol(ProtocolError),
}

/// Errors that are specific to the MQTT protocol implementation.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ProtocolError {
    Bounds,
    DataSize,
    Invalid,
    Failed,
    PacketSize,
    MalformedPacket,
    MalformedInteger,
    UnknownProperty,
    UnsupportedPacket,
    BufferSize,
    InvalidProperty,
}

impl<E> From<E> for Error<E> {
    fn from(e: E) -> Error<E> {
        Error::Network(e)
    }
}

impl<T, N, C> MqttClient<T, N, C>
where
    T: ArrayLength<u8>,
    N: embedded_nal::TcpStack,
    C: Clock,
    u32: TryFrom<C::T>,
{
    /// Construct a new MQTT client.
    ///
    /// # Args
    /// * `broker` - The IP address of the broker to connect to.
    /// * `client_id` The client ID to use for communicating with the broker. If empty, rely on the
    ///   broker to automatically assign a client ID.
    /// * `network_stack` - The network stack to use for communication.
    ///
    /// # Returns
    /// An `MqttClient` that can be used for publishing messages and subscribing to topics.
    pub fn new(
        broker: IpAddr,
        client_id: &str,
        network_stack: N,
        clock: C,
    ) -> Result<Self, Error<N::Error>> {
        let session_state = SessionState::new(
            broker,
            String::from_str(client_id).or(Err(Error::ProvidedClientIdTooLong))?,
        );

        let mut client = MqttClient {
            network_stack: network_stack,
            socket: RefCell::new(None),
            state: RefCell::new(session_state),
            transmit_buffer: RefCell::new(GenericArray::default()),
            packet_reader: PacketReader::new(),
            connect_sent: false,
            clock: clock,
            ping_response_timeout: Seconds(2),
        };
        client.reset()?;
        Ok(client)
    }

    fn read(&self, mut buf: &mut [u8]) -> Result<usize, Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let mut socket = socket_ref.take().unwrap();
        let read = match self.network_stack.read(&mut socket, &mut buf) {
            Ok(count) => count,
            Err(nb::Error::WouldBlock) => 0,
            Err(nb::Error::Other(error)) => return Err(Error::Network(error)),
        };

        // Put the socket back into the option.
        socket_ref.replace(socket);

        Ok(read)
    }

    fn write(&self, state: &mut SessionState<C>, buf: &[u8]) -> Result<(), Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let mut socket = socket_ref.take().unwrap();
        state.last_write_time.replace(self.clock.try_now().unwrap());
        let written = nb::block!(self.network_stack.write(&mut socket, &buf))?;

        // Put the socket back into the option.
        socket_ref.replace(socket);

        if written != buf.len() {
            Err(Error::WriteFail)
        } else {
            Ok(())
        }
    }

    /// Subscribe to a topic.
    ///
    /// # Note
    /// A subscription is not maintained across a disconnection with the broker. In the case of MQTT
    /// disconnections, topics will need to be subscribed to again.
    ///
    /// # Args
    /// * `topic` - The topic to subscribe to.
    /// * `properties` - A list of properties to attach to the subscription request. May be empty.
    pub fn subscribe<'a, 'b>(
        &self,
        topic: &'a str,
        properties: &[Property<'b>],
    ) -> Result<(), Error<N::Error>> {
        let mut state = self.state.borrow_mut();

        if !self.connect_sent {
            return Err(Error::NotReady);
        }
        let packet_id = state.get_packet_identifier();

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::subscribe_message(&mut buffer, topic, packet_id, properties)
            .map_err(|e| Error::Protocol(e))?;

        match self.write(&mut state, packet) {
            Ok(_) => {
                info!("Subscribing to `{}`: {}", topic, packet_id);
                state
                    .pending_subscriptions
                    .push(packet_id)
                    .map_err(|_| Error::Unsupported)?;
                state.increment_packet_identifier();
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Determine if any subscriptions are waiting for completion.
    ///
    /// # Returns
    /// True if any subscriptions are waiting for confirmation from the broker.
    pub fn subscriptions_pending(&self) -> bool {
        self.state.borrow().pending_subscriptions.len() != 0
    }

    /// Determine if the client has established a connection with the broker.
    ///
    /// # Returns
    /// True if the client is connected to the broker.
    pub fn is_connected(&self) -> Result<bool, Error<N::Error>> {
        Ok(self.socket_is_connected()? && self.connect_sent)
    }

    /// Publish a message over MQTT.
    ///
    /// # Note
    /// If the client is not yet connected to the broker, the message will be silently ignored.
    ///
    /// # Note
    /// Currently, Only QoS level 1 (at most once) delivery is supported.
    ///
    /// # Args
    /// * `topic` - The topic to publish the message to.
    /// * `data` - The data to transmit as the message contents.
    /// * `qos` - The desired quality-of-service level of the message. Must be QoS::AtMostOnce
    /// * `properties` - A list of properties to associate with the message being published. May be
    ///   empty.
    pub fn publish<'a, 'b>(
        &self,
        topic: &'a str,
        data: &[u8],
        qos: QoS,
        properties: &[Property<'a>],
    ) -> Result<(), Error<N::Error>> {
        // TODO: QoS support.
        assert!(qos == QoS::AtMostOnce);

        // If we are not yet connected to the broker, we can't transmit a message.
        if self.is_connected()? == false {
            return Ok(());
        }

        debug!(
            "Publishing to `{}`: {:?} Props: {:?}",
            topic, data, properties
        );

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::publish_message(&mut buffer, topic, data, properties)
            .map_err(|e| Error::Protocol(e))?;
        self.write(&mut self.state.borrow_mut(), packet)
    }

    fn socket_is_connected(&self) -> Result<bool, N::Error> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take();
        if socket.is_none() {
            return Ok(false);
        }
        let socket = socket.unwrap();

        let connected = self.network_stack.is_connected(&socket)?;

        socket_ref.replace(socket);

        Ok(connected)
    }

    fn reset(&mut self) -> Result<(), Error<N::Error>> {
        self.state.borrow_mut().reset();
        self.packet_reader.reset();
        self.connect_sent = false;

        Ok(())
    }

    /// Non-graceful socket shutdown (will cause broker to send out LWTT if configured,
    /// etc.).
    fn shutdown_socket(&mut self) -> Result<(), Error<N::Error>> {
        self.network_stack
            .close(self.socket.borrow_mut().take().unwrap())?;
        self.reset()?;
        Ok(())
    }

    fn connect_to_broker(&mut self) -> Result<(), Error<N::Error>> {
        self.reset()?;

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::connect_message(
            &mut buffer,
            self.state.borrow().client_id.as_str().as_bytes(),
            *self.state.borrow().keep_alive_interval.integer() as u16,
            &[Property::MaximumPacketSize(
                self.packet_reader.maximum_packet_length() as u32,
            )],
        )
        .map_err(|e| Error::Protocol(e))?;

        info!("Sending CONNECT");
        self.write(&mut self.state.borrow_mut(), packet)?;

        self.connect_sent = true;

        Ok(())
    }

    fn handle_packet<'p, F>(
        &self,
        packet: ReceivedPacket<'p>,
        f: &mut F,
    ) -> Result<(), Error<N::Error>>
    where
        for<'a> F: FnMut(&Self, &'a str, &[u8], &[Property<'a>]),
    {
        if !self.state.borrow().connected {
            let mut state = self.state.borrow_mut();
            if let ReceivedPacket::ConnAck(acknowledge) = packet {
                if acknowledge.reason_code != 0 {
                    return Err(Error::Failed(acknowledge.reason_code));
                }

                if acknowledge.session_present {
                    // We do not currently support saved session state.
                    return Err(Error::Unsupported);
                }

                state.connected = true;

                for property in acknowledge.properties {
                    match property {
                        Property::MaximumPacketSize(size) => {
                            state.maximum_packet_size.replace(size);
                        }
                        Property::AssignedClientIdentifier(id) => {
                            state.client_id =
                                String::from_str(id).or(Err(Error::ProvidedClientIdTooLong))?;
                        }
                        Property::ServerKeepAlive(keep_alive) => {
                            state.keep_alive_interval = Seconds(keep_alive as u32);
                        }
                        _prop => info!("Ignoring property: {:?}", _prop),
                    };
                }

                return Ok(());
            } else {
                // It is a protocol error to receive anything else when not connected.
                // TODO: Verify it is a protocol error.
                error!(
                    "Received invalid packet outside of connected state: {:?}",
                    packet
                );
                return Err(Error::Protocol(ProtocolError::Invalid));
            }
        }

        match packet {
            ReceivedPacket::Publish(info) => {
                // Call a handler function to deal with the received data.
                let payload = self
                    .packet_reader
                    .payload()
                    .map_err(|e| Error::Protocol(e))?;
                f(self, info.topic, payload, &info.properties);

                Ok(())
            }

            ReceivedPacket::SubAck(subscribe_acknowledge) => {
                let mut state = self.state.borrow_mut();
                match state
                    .pending_subscriptions
                    .iter()
                    .position(|v| *v == subscribe_acknowledge.packet_identifier)
                {
                    None => {
                        error!("Got bad suback: {:?}", subscribe_acknowledge);
                        return Err(Error::Protocol(ProtocolError::Invalid));
                    }
                    Some(index) => state.pending_subscriptions.swap_remove(index),
                };

                if subscribe_acknowledge.reason_code != 0 {
                    return Err(Error::Failed(subscribe_acknowledge.reason_code));
                }

                Ok(())
            }

            ReceivedPacket::PingResp => {
                self.state.borrow_mut().pending_pingreq_time.take();
                Ok(())
            }

            _ => Err(Error::Unsupported),
        }
    }

    fn open_socket(&mut self) -> Result<(), N::Error> {
        let socket = self.network_stack.open(Mode::NonBlocking)?;
        self.socket.borrow_mut().replace(socket);
        Ok(())
    }

    fn connect_socket(&mut self) -> Result<(), Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take().unwrap();

        // Connect to the broker's TCP port with a new socket.
        // TODO: Limit the time between connect attempts to prevent network spam.
        let socket = self
            .network_stack
            .connect(socket, SocketAddr::new(self.state.borrow().broker, 1883))?;

        // Store the new socket for future use.
        socket_ref.replace(socket);

        Ok(())
    }

    fn is_ping_request_due(&self) -> bool {
        let s = self.state.borrow();
        if s.keep_alive_interval == Seconds(0_u32) {
            return false;
        }
        // Note(unwrap): We should have at least sent out the initial connect packet.
        let last_write_time = &s.last_write_time.unwrap();
        // On error (clock wrapped) be conservative and send out a keepalive ping.
        let now = self.clock.try_now().unwrap();
        now.checked_duration_since(last_write_time)
            .and_then(|diff| Seconds::<u32>::try_from(diff).ok())
            .map(|diff| diff >= s.keep_alive_interval)
            .unwrap_or(true)
    }

    fn send_ping_request(&mut self) -> Result<(), Error<N::Error>> {
        debug!("Sending keepalive ping request");
        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::ping_req_message(&mut buffer).map_err(|e| Error::Protocol(e))?;
        self.write(&mut self.state.borrow_mut(), packet)?;
        self.state
            .borrow_mut()
            .pending_pingreq_time
            .replace(self.clock.try_now().unwrap());
        Ok(())
    }

    fn ping_response_is_overdue(&self) -> bool {
        let s = self.state.borrow();
        match &s.pending_pingreq_time {
            None => false,
            Some(sent) => {
                // Be conservative on error (clock wrapped) and just wait until the next ping
                // to detect a lost connection.
                let now = self.clock.try_now().unwrap();
                now.checked_duration_since(sent)
                    .and_then(|diff| Seconds::<u32>::try_from(diff).ok())
                    .map(|diff| diff >= self.ping_response_timeout)
                    .unwrap_or(false)
            }
        }
    }

    /// Check the MQTT interface for available messages, and send a keepalive ping if
    /// required.
    ///
    /// Should be called at least once per second, as per the configured Clock, for
    /// timely handling of keepalive pings. (More infrequent calls can also cause
    /// pings to be missed entirely when the keepalive interval is set to a large value
    /// and the clock wraps around.)
    ///
    /// # Args
    /// * `f` - A closure to process any received messages. The closure should accept the client,
    /// topic, message, and list of proprties (in that order).
    pub fn poll<F>(&mut self, mut f: F) -> Result<(), Error<N::Error>>
    where
        for<'a> F: FnMut(&Self, &'a str, &[u8], &[Property<'a>]),
    {
        // If the socket is not connected, we can't do anything.
        if self.socket_is_connected()? == false {
            let result = if self.connect_sent {
                Err(Error::Disconnected)
            } else {
                Ok(())
            };
            if self.socket.borrow().is_none() {
                self.open_socket()?;
            }
            self.reset()?;
            self.connect_socket()?;
            return result;
        }

        // Connect to the MQTT broker.
        if !self.connect_sent {
            self.connect_to_broker()?;
            return Ok(());
        }

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.read(&mut buf)?;
        if received > 0 {
            debug!("Received {} bytes", received);
        }

        let mut processed = 0;
        while processed < received {
            match self.packet_reader.slurp(&buf[processed..received]) {
                Ok(count) => {
                    debug!("Processed {} bytes", count);
                    processed += count
                }

                Err(_) => {
                    // TODO: We should handle recoverable errors better.
                    self.reset()?;
                    return Err(Error::Disconnected);
                }
            }

            // Handle any received packets.
            while self.packet_reader.packet_available() {
                // TODO: Handle deserialize errors properly.
                let packet = ReceivedPacket::parse_message(&self.packet_reader)
                    .map_err(|e| Error::Protocol(e))?;

                info!("Received {:?}", packet);
                self.handle_packet(packet, &mut f)?;
                self.packet_reader
                    .pop_packet()
                    .map_err(|e| Error::Protocol(e))?;
            }
        }

        if self.ping_response_is_overdue() {
            error!(
                "Expected ping response, but none received; dropping broker \
                    connection (timeout: {})",
                self.ping_response_timeout
            );
            self.shutdown_socket()?;
        }

        if self.is_ping_request_due() {
            self.send_ping_request()?;
        }

        Ok(())
    }
}
