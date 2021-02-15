use crate::{
    de::{deserialize::ReceivedPacket, PacketReader},
    ser::serialize,
    session_state::SessionState,
    Property, {debug, error, info},
};

use embedded_nal::{nb, IpAddr, SocketAddr};

use generic_array::{ArrayLength, GenericArray};
use heapless::String;

/// A client for interacting with an MQTT Broker.
pub struct MqttClient<T, N>
where
    T: ArrayLength<u8>,
    N: embedded_nal::TcpClientStack,
{
    network_stack: N,
    socket: Option<N::TcpSocket>,
    state: SessionState,
    packet_reader: PacketReader<T>,
    transmit_buffer: Option<GenericArray<u8, T>>,
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
    N: embedded_nal::TcpClientStack,
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
        mut network_stack: N,
    ) -> Result<Self, Error<N::Error>> {
        // Connect to the broker's TCP port.
        let mut socket = network_stack.socket()?;
        nb::block!(network_stack.connect(&mut socket, SocketAddr::new(broker, 1883)))?;

        let mut client = MqttClient {
            network_stack: network_stack,
            socket: Some(socket),
            state: SessionState::new(broker, client_id),
            transmit_buffer: Some(GenericArray::default()),
            packet_reader: PacketReader::new(),
            connect_sent: false,
        };

        client.reset()?;

        Ok(client)
    }

    fn read(&mut self, mut buf: &mut [u8]) -> Result<usize, Error<N::Error>> {
        // Atomically access the socket.
        let mut socket = self.socket.take().unwrap();
        let result = self.network_stack.receive(&mut socket, &mut buf);
        self.socket.replace(socket);

        result.or_else(|err| {
            match err {
                nb::Error::WouldBlock => Ok(0),
                nb::Error::Other(err) => Err(Error::Network(err)),
            }
        })
    }

    fn write(&mut self, buf: &[u8]) -> Result<(), Error<N::Error>> {
        // Atomically access the socket.
        let mut socket = self.socket.take().unwrap();
        let result = nb::block!(self.network_stack.send(&mut socket, &buf)).map_err(|err|
                Error::Network(err));
        self.socket.replace(socket);

        result.and_then(|written| {
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
        if !self.connect_sent {
            return Err(Error::NotReady);
        }

        let packet_id = self.state.get_packet_identifier();

        // Atomically access our buffer.
        let mut buffer = self.transmit_buffer.take().unwrap();
        let packet = match serialize::subscribe_message(&mut buffer, topic, packet_id, properties).map_err(|e| Error::Protocol(e)) {
            Ok(packet) => packet,
            Err(other) => {
                self.transmit_buffer.replace(buffer);
                return Err(other);
            },
        };

        let result = self.write(packet);
        self.transmit_buffer.replace(buffer);

        result.and_then(|_| {
            info!("Subscribing to `{}`: {}", topic, packet_id);
            self.state
                .pending_subscriptions
                .push(packet_id)
                .map_err(|_| Error::Unsupported)?;
            self.state.increment_packet_identifier();
            Ok(())
        })
    }

    /// Determine if any subscriptions are waiting for completion.
    ///
    /// # Returns
    /// True if any subscriptions are waiting for confirmation from the broker.
    pub fn subscriptions_pending(&self) -> bool {
        self.state.pending_subscriptions.len() != 0
    }

    /// Determine if the client has established a connection with the broker.
    ///
    /// # Returns
    /// True if the client is connected to the broker.
    pub fn is_connected(&mut self) -> Result<bool, Error<N::Error>> {
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

        let mut buffer = self.transmit_buffer.take().unwrap();
        let packet = match serialize::publish_message(&mut buffer, topic, data, properties) .map_err(|e| Error::Protocol(e)) {
            Ok(packet) => packet,
            Err(other) => {
                self.transmit_buffer.replace(buffer);
                return Err(other);
            },
        };

        self.write(packet)
    }

    fn socket_is_connected(&mut self) -> Result<bool, N::Error> {
        let socket = self.socket.take().unwrap();
        let connected = self.network_stack.is_connected(&socket)?;
        self.socket.replace(socket);

        Ok(connected)
    }

    fn reset(&mut self) -> Result<(), Error<N::Error>> {
        self.state.reset();
        self.packet_reader.reset();
        self.connect_sent = false;

        Ok(())
    }

    fn connect_to_broker(&mut self) -> Result<(), Error<N::Error>> {
        self.reset()?;

        let mut buffer = self.transmit_buffer.take().unwrap();

        let packet = match serialize::connect_message(
            &mut buffer,
            self.state.client_id.as_str().as_bytes(),
            self.state.keep_alive_interval,
            &[Property::MaximumPacketSize(
                self.packet_reader.maximum_packet_length() as u32,
            )],
        )
        .map_err(|e| Error::Protocol(e)) {
            Ok(packet) => packet,
            Err(other) => {
                self.transmit_buffer.replace(buffer);
                return Err(other);
            },
        };

        info!("Sending CONNECT");
        self.write(packet)?;

        self.connect_sent = true;

        Ok(())
    }

    fn connect_socket(&mut self) -> Result<(), Error<N::Error>> {
        let mut socket = self.socket.take().unwrap();

        // Connect to the broker's TCP port with a new socket.
        // TODO: Limit the time between connect attempts to prevent network spam.
        let result = nb::block!(self.network_stack.connect(&mut socket,
                                                           SocketAddr::new(self.state.broker,
                                                               1883))).map_err(|err|
                                                           Error::Network(err));

        self.socket.replace(socket);

        result
    }

    /// Check the MQTT interface for available messages.
    ///
    /// # Args
    /// * `f` - A closure to process any received messages. The closure should accept the client,
    /// topic, message, and list of proprties (in that order).
    pub fn poll<F>(&mut self, mut f: F) -> Result<(), Error<N::Error>>
    where
        for<'a> F: FnMut(&mut Self, &'a str, &[u8], &[Property<'a>]),
    {
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
                let packet = ReceivedPacket::parse_message(&self.packet_reader)
                    .map_err(|e| Error::Protocol(e))?;

                info!("Received {:?}", packet);

                // Handle packet receive.
                if !self.state.connected {
                    if let ReceivedPacket::ConnAck(acknowledge) = packet {
                        if acknowledge.reason_code != 0 {
                            return Err(Error::Failed(acknowledge.reason_code));
                        }

                        if acknowledge.session_present {
                            // We do not currently support saved session state.
                            return Err(Error::Unsupported);
                        }

                        self.state.connected = true;

                        for property in acknowledge.properties {
                            match property {
                                Property::MaximumPacketSize(size) => {
                                    self.state.maximum_packet_size.replace(size);
                                }
                                Property::AssignedClientIdentifier(id) => {
                                    self.state.client_id = String::from(id);
                                }
                                Property::ServerKeepAlive(keep_alive) => {
                                    self.state.keep_alive_interval = keep_alive;
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
                    ReceivedPacket::Publish(ref info) => {
                        // Call a handler function to deal with the received data.
                        let payload = self
                            .packet_reader
                            .payload()
                            .map_err(|e| Error::Protocol(e))?;
                        f(&mut self, info.topic, payload, &info.properties);
                    }

                    ReceivedPacket::SubAck(ref subscribe_acknowledge) => {
                        match self.state
                            .pending_subscriptions
                            .iter()
                            .position(|v| *v == subscribe_acknowledge.packet_identifier)
                        {
                            None => {
                                error!("Got bad suback: {:?}", subscribe_acknowledge);
                                return Err(Error::Protocol(ProtocolError::Invalid));
                            }
                            Some(index) => self.state.pending_subscriptions.swap_remove(index),
                        };

                        if subscribe_acknowledge.reason_code != 0 {
                            return Err(Error::Failed(subscribe_acknowledge.reason_code));
                        }
                    }

                    _ => return Err(Error::Unsupported),
                }

                // Packet is no longer necessary.
                core::mem::drop(packet);

                self.packet_reader
                    .pop_packet()
                    .map_err(|e| Error::Protocol(e))?;
            }
        }

        Ok(())
    }
}
