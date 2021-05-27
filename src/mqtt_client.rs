use crate::{
    de::{deserialize::ReceivedPacket, PacketReader},
    ser::serialize,
    session_state::SessionState,
    Property, {debug, error, info},
};

use embedded_nal::{nb, IpAddr, SocketAddr, TcpClientStack};

use heapless::String;

use core::str::FromStr;

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
    Unsupported,
    ProvidedClientIdTooLong,
    Failed(u8),
    Protocol(ProtocolError),
    SessionReset,
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

/// The general structure for managing MQTT via Minimq.
pub struct Minimq<N, const T: usize>
where
    N: TcpClientStack,
{
    pub client: MqttClient<N, T>,
    packet_reader: PacketReader<T>,
}

/// A client for interacting with an MQTT Broker.
pub struct MqttClient<N: TcpClientStack, const T: usize> {
    network_stack: N,
    socket: Option<N::TcpSocket>,
    session_state: SessionState,
    connect_sent: bool,
}

impl<N, const T: usize> MqttClient<N, T>
where
    N: TcpClientStack,
{
    fn read(&mut self, mut buf: &mut [u8]) -> Result<usize, Error<N::Error>> {
        // Atomically access the socket.
        let socket = self.socket.as_mut().unwrap();
        let result = self.network_stack.receive(socket, &mut buf);

        result.or_else(|err| match err {
            nb::Error::WouldBlock => Ok(0),
            nb::Error::Other(err) => Err(Error::Network(err)),
        })
    }

    fn write(&mut self, buf: &[u8]) -> Result<(), Error<N::Error>> {
        let socket = self.socket.as_mut().unwrap();
        self.network_stack
            .send(socket, &buf)
            .map_err(|err| match err {
                nb::Error::WouldBlock => Error::WriteFail,
                nb::Error::Other(err) => Error::Network(err),
            })
            .and_then(|written| {
                if written != buf.len() {
                    Err(Error::WriteFail)
                } else {
                    Ok(())
                }
            })
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
        &mut self,
        topic: &'a str,
        properties: &[Property<'b>],
    ) -> Result<(), Error<N::Error>> {
        if !self.session_state.connected {
            return Err(Error::NotReady);
        }

        let packet_id = self.session_state.get_packet_identifier();

        let mut buffer: [u8; T] = [0; T];
        let packet = serialize::subscribe_message(&mut buffer, topic, packet_id, properties)
            .map_err(|e| Error::Protocol(e))?;

        self.write(packet).and_then(|_| {
            info!("Subscribing to `{}`: {}", topic, packet_id);
            self.session_state
                .pending_subscriptions
                .push(packet_id)
                .map_err(|_| Error::Unsupported)?;
            self.session_state.increment_packet_identifier();
            Ok(())
        })
    }

    /// Determine if any subscriptions are waiting for completion.
    ///
    /// # Returns
    /// True if any subscriptions are waiting for confirmation from the broker.
    pub fn subscriptions_pending(&self) -> bool {
        self.session_state.pending_subscriptions.len() != 0
    }

    /// Determine if the client has established a connection with the broker.
    ///
    /// # Returns
    /// True if the client is connected to the broker.
    pub fn is_connected(&mut self) -> Result<bool, Error<N::Error>> {
        Ok(self.socket_is_connected()? && self.session_state.connected)
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
        &mut self,
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

        let mut buffer: [u8; T] = [0; T];
        let packet = serialize::publish_message(&mut buffer, topic, data, properties)
            .map_err(|e| Error::Protocol(e))?;
        self.write(packet)
    }

    fn socket_is_connected(&mut self) -> Result<bool, N::Error> {
        if self.socket.is_none() {
            let socket = self.network_stack.socket()?;
            self.socket.replace(socket);
        }

        let socket = self.socket.as_ref().unwrap();
        self.network_stack.is_connected(socket)
    }

    fn reset(&mut self) {
        self.connect_sent = false;
        self.session_state.connected = false;
    }

    fn connect_to_broker(&mut self) -> Result<(), Error<N::Error>> {
        self.reset();

        let properties = [
            // Tell the broker our maximum packet size.
            Property::MaximumPacketSize(T as u32),
            // The session does not expire.
            Property::SessionExpiryInterval(u32::MAX),
        ];

        let mut buffer: [u8; T] = [0; T];
        let packet = serialize::connect_message(
            &mut buffer,
            self.session_state.client_id.as_str().as_bytes(),
            self.session_state.keep_alive_interval,
            &properties,
            // Only perform a clean start if we do not have any session state.
            !self.session_state.is_present(),
        )
        .map_err(|e| Error::Protocol(e))?;

        info!("Sending CONNECT");
        self.write(packet)?;

        self.connect_sent = true;

        Ok(())
    }

    fn connect_socket(&mut self) -> Result<(), Error<N::Error>> {
        let socket = self.socket.as_mut().unwrap();

        // Connect to the broker's TCP port with a new socket.
        // TODO: Limit the time between connect attempts to prevent network spam.
        self.network_stack
            .connect(socket, SocketAddr::new(self.session_state.broker, 1883))
            .map_err(|err| match err {
                nb::Error::WouldBlock => Error::WriteFail,
                nb::Error::Other(err) => Error::Network(err),
            })
    }

    fn handle_packet<'a, F>(
        &mut self,
        packet: ReceivedPacket<'a>,
        f: &mut F,
    ) -> Result<(), Error<N::Error>>
    where
        F: FnMut(&mut MqttClient<N, T>, &'a str, &[u8], &[Property<'a>]),
    {
        if !self.session_state.connected {
            if let ReceivedPacket::ConnAck(acknowledge) = packet {
                let mut result = Ok(());

                if acknowledge.reason_code != 0 {
                    return Err(Error::Failed(acknowledge.reason_code));
                }

                if !acknowledge.session_present {
                    if self.session_state.connected {
                        result = Err(Error::SessionReset);
                    }

                    // Reset the session state upon connection with a broker that doesn't have a
                    // session state saved for us.
                    self.session_state.reset();
                }

                // Now that we are connected, we have session state that will be persisted.
                self.session_state.register_connection();
                self.session_state.connected = true;

                for property in acknowledge.properties {
                    match property {
                        Property::MaximumPacketSize(size) => {
                            self.session_state.maximum_packet_size.replace(size);
                        }
                        Property::AssignedClientIdentifier(id) => {
                            self.session_state.client_id =
                                String::from_str(id).or(Err(Error::ProvidedClientIdTooLong))?;
                        }
                        Property::ServerKeepAlive(keep_alive) => {
                            self.session_state.keep_alive_interval = keep_alive;
                        }
                        _prop => info!("Ignoring property: {:?}", _prop),
                    };
                }

                return result;
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
                f(self, info.topic, info.payload, &info.properties);

                Ok(())
            }

            ReceivedPacket::SubAck(subscribe_acknowledge) => {
                match self
                    .session_state
                    .pending_subscriptions
                    .iter()
                    .position(|v| *v == subscribe_acknowledge.packet_identifier)
                {
                    None => {
                        error!("Got bad suback: {:?}", subscribe_acknowledge);
                        return Err(Error::Protocol(ProtocolError::Invalid));
                    }
                    Some(index) => self.session_state.pending_subscriptions.swap_remove(index),
                };

                if subscribe_acknowledge.reason_code != 0 {
                    return Err(Error::Failed(subscribe_acknowledge.reason_code));
                }

                Ok(())
            }

            _ => Err(Error::Unsupported),
        }
    }
}

impl<N: TcpClientStack, const T: usize> Minimq<N, T> {
    /// Construct a new MQTT client.
    ///
    /// # Args
    /// * `broker` - The IP address of the broker to connect to.
    /// * `client_id` The client ID to use for communicating with the broker. If empty, rely on the
    ///   broker to automatically assign a client ID.
    /// * `network_stack` - The network stack to use for communication.
    ///
    /// # Returns
    /// A `Minimq` interface that can be used for publishing messages and subscribing to topics.
    pub fn new(broker: IpAddr, client_id: &str, network_stack: N) -> Result<Self, Error<N::Error>> {
        let session_state = SessionState::new(
            broker,
            String::from_str(client_id).or(Err(Error::ProvidedClientIdTooLong))?,
        );

        let minimq = Minimq {
            client: MqttClient {
                network_stack,
                socket: None,
                session_state,
                connect_sent: false,
            },
            packet_reader: PacketReader::new(),
        };

        Ok(minimq)
    }

    /// Check the MQTT interface for available messages.
    ///
    /// # Args
    /// * `f` - A closure to process any received messages. The closure should accept the client,
    /// topic, message, and list of proprties (in that order).
    pub fn poll<F>(&mut self, mut f: F) -> Result<(), Error<N::Error>>
    where
        for<'a> F: FnMut(&mut MqttClient<N, T>, &'a str, &[u8], &[Property<'a>]),
    {
        // If the socket is not connected, we can't do anything.
        if self.client.socket_is_connected()? == false {
            self.client.reset();
            self.packet_reader.reset();
            self.client.connect_socket()?;
            return Ok(());
        }

        // Connect to the MQTT broker.
        if !self.client.connect_sent {
            self.client.connect_to_broker()?;
            return Ok(());
        }

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.client.read(&mut buf)?;
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

                Err(e) => {
                    self.client.reset();
                    self.packet_reader.reset();
                    return Err(Error::Protocol(e));
                }
            }

            // Handle any received packets.
            while self.packet_reader.packet_available() {
                let packet = ReceivedPacket::parse_message(&self.packet_reader)
                    .map_err(|e| Error::Protocol(e))?;

                info!("Received {:?}", packet);

                let result = self.client.handle_packet(packet, &mut f);

                self.packet_reader
                    .pop_packet()
                    .map_err(|e| Error::Protocol(e))?;

                // If there was an error, return it now. Note that we ensure the packet is removed
                // from buffering after processing even in error conditions..
                result?;
            }
        }

        Ok(())
    }
}
