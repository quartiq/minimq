use crate::{
    de::{deserialize::ReceivedPacket, PacketReader},
    ser::serialize,
    session_state::SessionState,
    Property, {debug, error, info},
};

use core::cell::RefCell;

use embedded_nal::{nb, IpAddr, Mode, SocketAddr};

use generic_array::{ArrayLength, GenericArray};
use heapless::String;

/// A client for interacting with an MQTT Broker.
pub struct MqttClient<T, N>
where
    T: ArrayLength<u8>,
    N: embedded_nal::TcpStack,
{
    /// The network stack originally provided to the client.
    pub network_stack: N,

    socket: RefCell<Option<N::TcpSocket>>,
    state: RefCell<SessionState>,
    packet_reader: PacketReader<T>,
    transmit_buffer: RefCell<GenericArray<u8, T>>,
    connect_sent: bool,
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

impl<T, N> MqttClient<T, N>
where
    T: ArrayLength<u8>,
    N: embedded_nal::TcpStack,
{
    /// Construct a new MQTT client.
    ///
    /// # Args
    /// * `broker` - The IP address of the broker to connect to.
    /// * `client_id` The client ID to use for communicating with the broker.
    /// * `network_stack` - The network stack to use for communication.
    ///
    /// # Returns
    /// An `MqttClient` that can be used for publishing messages and subscribing to topics.
    pub fn new<'a>(
        broker: IpAddr,
        client_id: &'a str,
        network_stack: N,
    ) -> Result<Self, Error<N::Error>> {
        // Connect to the broker's TCP port.
        let socket = network_stack.open(Mode::NonBlocking)?;

        // Next, connect to the broker over MQTT.
        let socket = network_stack.connect(socket, SocketAddr::new(broker, 1883))?;

        let mut client = MqttClient {
            network_stack: network_stack,
            socket: RefCell::new(Some(socket)),
            state: RefCell::new(SessionState::new(broker, client_id)),
            transmit_buffer: RefCell::new(GenericArray::default()),
            packet_reader: PacketReader::new(),
            connect_sent: false,
        };

        client.reset()?;

        Ok(client)
    }

    fn read(&self, mut buf: &mut [u8]) -> Result<usize, Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let mut socket = socket_ref.take().unwrap();
        let read = nb::block!(self.network_stack.read(&mut socket, &mut buf)).unwrap();

        // Put the socket back into the option.
        socket_ref.replace(socket);

        Ok(read)
    }

    fn write(&self, buf: &[u8]) -> Result<(), Error<N::Error>> {
        let mut socket_ref = self.socket.borrow_mut();
        let mut socket = socket_ref.take().unwrap();
        let written = nb::block!(self.network_stack.write(&mut socket, &buf)).unwrap();

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

        match self.write(packet) {
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
    /// True if the client has sent a request to establish an MQTT connection.
    pub fn is_connected(&self) -> bool {
        self.connect_sent
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
        if self.is_connected() == false {
            return Ok(());
        }

        info!(
            "Publishing to `{}`: {:?} Props: {:?}",
            topic, data, properties
        );

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::publish_message(&mut buffer, topic, data, properties)
            .map_err(|e| Error::Protocol(e))?;
        self.write(packet)
    }

    fn socket_is_connected(&self) -> Result<bool, N::Error> {
        let mut socket_ref = self.socket.borrow_mut();
        let socket = socket_ref.take().unwrap();

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

    fn connect_to_broker(&mut self) -> Result<(), Error<N::Error>> {
        self.reset()?;

        let mut buffer = self.transmit_buffer.borrow_mut();
        let packet = serialize::connect_message(
            &mut buffer,
            self.state.borrow().client_id.as_str().as_bytes(),
            self.state.borrow().keep_alive_interval,
            &[Property::MaximumPacketSize(
                self.packet_reader.maximum_packet_length() as u32,
            )],
        )
        .map_err(|e| Error::Protocol(e))?;

        info!("Sending CONNECT");
        self.write(packet)?;

        self.connect_sent = true;

        Ok(())
    }

    fn handle_packet<'p, F>(&self, packet: ReceivedPacket<'p>, f: &F) -> Result<(), Error<N::Error>>
    where
        for<'a> F: Fn(&Self, &'a str, &[u8], &[Property<'a>]),
    {
        let mut state = self.state.borrow_mut();
        if !state.connected {
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
                            state.client_id = String::from(id);
                        }
                        Property::ServerKeepAlive(keep_alive) => {
                            state.keep_alive_interval = keep_alive;
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

            _ => Err(Error::Unsupported),
        }
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

    /// Check the MQTT interface for available messages.
    ///
    /// # Args
    /// * `f` - A closure to process any received messages. The closure should accept the client,
    /// topic, message, and list of proprties (in that order).
    pub fn poll<F>(&mut self, f: F) -> Result<(), Error<N::Error>>
    where
        for<'a> F: Fn(&Self, &'a str, &[u8], &[Property<'a>]),
    {
        debug!("Polling MQTT interface");

        // If the socket is not connected, we can't do anything.
        if self.socket_is_connected()? == false {
            let result = if self.connect_sent {
                Err(Error::Disconnected)
            } else {
                Ok(())
            };
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
        debug!("Received {} bytes", received);

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
                self.handle_packet(packet, &f)?;
                self.packet_reader
                    .pop_packet()
                    .map_err(|e| Error::Protocol(e))?;
            }
        }

        Ok(())
    }
}
