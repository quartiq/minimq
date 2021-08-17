use crate::{
    de::{deserialize::ReceivedPacket, PacketReader},
    ser::serialize,
    session_state::SessionState,
    Property, {debug, error, info, warn},
};

use embedded_nal::{nb, IpAddr, SocketAddr, TcpClientStack};
use embedded_time::Clock;

use heapless::String;

use core::str::FromStr;

/// The default duration to wait for a ping response from the broker.
const PING_TIMEOUT: embedded_time::duration::Seconds = embedded_time::duration::Seconds(5);

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
#[derive(Debug, PartialEq)]
pub enum Error<E> {
    Network(E),
    WriteFail,
    NotReady,
    Unsupported,
    ProvidedClientIdTooLong,
    Failed(u8),
    Protocol(ProtocolError),
    SessionReset,
    Clock(embedded_time::clock::Error),
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

impl<E> From<embedded_time::clock::Error> for Error<E> {
    fn from(clock: embedded_time::clock::Error) -> Self {
        Error::Clock(clock)
    }
}

impl<E> From<ProtocolError> for Error<E> {
    fn from(error: ProtocolError) -> Self {
        Error::Protocol(error)
    }
}

mod sm {

    use smlang::statemachine;

    statemachine! {
        transitions: {
            *Restart + GotSocket = ConnectTransport,
            ConnectTransport + Connect = ConnectBroker,
            ConnectBroker + SentConnect = Active,
            ConnectBroker + Disconnect = Restart,
            Active + Disconnect = Restart,
        }
    }

    pub struct Context;

    impl StateMachineContext for Context {}
}

use sm::{Context, Events, StateMachine, States};

/// The general structure for managing MQTT via Minimq.
pub struct Minimq<N, C, const T: usize>
where
    N: TcpClientStack,
    C: Clock,
{
    pub client: MqttClient<N, C, T>,
    packet_reader: PacketReader<T>,
}

/// A client for interacting with an MQTT Broker.
pub struct MqttClient<N: TcpClientStack, C: Clock, const T: usize> {
    network_stack: N,
    clock: C,
    socket: Option<N::TcpSocket>,
    session_state: SessionState<C>,
    connection_state: StateMachine<Context>,
}

impl<N, C, const T: usize> MqttClient<N, C, T>
where
    N: TcpClientStack,
    C: Clock,
{
    fn process(&mut self) -> Result<(), Error<N::Error>> {
        // Potentially update the state machine depending on the current socket connection status.
        if !self
            .socket_is_connected()
            .map_err(|err| Error::Network(err))?
        {
            self.connection_state.process_event(Events::Disconnect).ok();
        } else {
            self.connection_state.process_event(Events::Connect).ok();
        }

        match self.connection_state.state() {
            // In the RESTART state, we need to reopen the TCP socket.
            &States::Restart => {
                if self.socket.is_some() {
                    self.network_stack
                        .close(self.socket.take().unwrap())
                        .map_err(|err| Error::Network(err))?;
                }

                // Allocate a new socket to use and begin connecting it.
                self.socket.replace(
                    self.network_stack
                        .socket()
                        .map_err(|err| Error::Network(err))?,
                );
                self.connection_state
                    .process_event(Events::GotSocket)
                    .unwrap();
            }

            // In the connect transport state, we need to connect our TCP socket to the broker.
            &States::ConnectTransport => {
                self.network_stack
                    .connect(
                        self.socket.as_mut().unwrap(),
                        SocketAddr::new(self.session_state.broker, 1883),
                    )
                    .map_err(|err| match err {
                        nb::Error::WouldBlock => Error::WriteFail,
                        nb::Error::Other(err) => Error::Network(err),
                    })?;
            }

            // Next, connect to the broker via the MQTT protocol.
            &States::ConnectBroker => {
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
                    self.session_state.keep_alive_interval.unwrap_or(0),
                    &properties,
                    // Only perform a clean start if we do not have any session state.
                    !self.session_state.is_present(),
                )?;

                info!("Sending CONNECT");
                self.write(packet)?;

                self.connection_state
                    .process_event(Events::SentConnect)
                    .unwrap();
            }

            _ => {}
        }

        self.handle_timers()?;

        Ok(())
    }

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
                    self.session_state
                        .register_transmission(self.clock.try_now()?);
                    Ok(())
                }
            })
    }

    /// Configure the MQTT keep-alive interval.
    ///
    /// # Note
    /// This must be completed before connecting to a broker.
    ///
    /// # Note
    /// The broker may override the requested keep-alive interval. Any value requested by the
    /// broker will be used instead.
    ///
    /// # Args
    /// * `interval` - The keep-alive interval in seconds. A ping will be transmitted if no other
    /// messages are sent within 50% of the keep-alive interval.
    pub fn set_keepalive_interval(&mut self, interval: u16) -> Result<(), Error<N::Error>> {
        match self.connection_state.state() {
            &States::Active => Err(Error::NotReady),
            _ => {
                self.session_state.keep_alive_interval.replace(interval);
                Ok(())
            }
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
        &mut self,
        topic: &'a str,
        properties: &[Property<'b>],
    ) -> Result<(), Error<N::Error>> {
        if !self.session_state.connected {
            return Err(Error::NotReady);
        }

        let packet_id = self.session_state.get_packet_identifier();

        let mut buffer: [u8; T] = [0; T];
        let packet = serialize::subscribe_message(&mut buffer, topic, packet_id, properties)?;

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
        Ok(self
            .socket_is_connected()
            .map_err(|err| Error::Network(err))?
            && self.session_state.connected)
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
        let packet = serialize::publish_message(&mut buffer, topic, data, properties)?;
        self.write(packet)
    }

    fn socket_is_connected(&mut self) -> Result<bool, N::Error> {
        if self.socket.is_none() {
            return Ok(false);
        }

        let socket = self.socket.as_ref().unwrap();
        self.network_stack.is_connected(socket)
    }

    fn reset(&mut self) {
        self.session_state.connected = false;
    }

    fn handle_packet<'a, F>(
        &mut self,
        packet: ReceivedPacket<'a>,
        f: &mut F,
    ) -> Result<(), Error<N::Error>>
    where
        F: FnMut(&mut MqttClient<N, C, T>, &'a str, &[u8], &[Property<'a>]),
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
                            self.session_state.keep_alive_interval.replace(keep_alive);
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

            ReceivedPacket::PingResp => {
                // Cancel the ping response timeout.
                if self.session_state.ping_timeout.is_some() {
                    self.session_state.ping_timeout = None;
                } else {
                    warn!("Got unexpected ping response");
                }

                Ok(())
            }

            _ => Err(Error::Unsupported),
        }
    }

    fn handle_timers(&mut self) -> Result<(), Error<N::Error>> {
        // If we are not connected, there's no session state to manage.
        if self.connection_state.state() != &States::Active {
            return Ok(());
        }

        let now = self.clock.try_now()?;

        if let Some(timeout) = self.session_state.ping_timeout {
            if now > timeout {
                // Reset network connection.
                self.connection_state.process_event(Events::Disconnect).ok();
            }
        } else {
            // Check if we need to ping the server.
            if self.session_state.ping_is_due(&now) {
                let mut buffer: [u8; T] = [0; T];

                // Note: The ping timeout is set at this point so that it's running even if we fail
                // to write the ping message. This is intentional incase the underlying transport
                // mechanism has stalled. The ping timeout will then allow us to recover the
                // underlying TCP connection.
                self.session_state.ping_timeout.replace(now + PING_TIMEOUT);

                let packet = serialize::ping_req_message(&mut buffer)?;
                self.write(packet)?;
            }
        }

        Ok(())
    }
}

impl<N: TcpClientStack, C: Clock, const T: usize> Minimq<N, C, T> {
    /// Construct a new MQTT interface.
    ///
    /// # Args
    /// * `broker` - The IP address of the broker to connect to.
    /// * `client_id` The client ID to use for communicating with the broker. If empty, rely on the
    ///   broker to automatically assign a client ID.
    /// * `network_stack` - The network stack to use for communication.
    /// * `clock` - The clock to use for managing MQTT state timing.
    ///
    /// # Returns
    /// A `Minimq` object that can be used for publishing messages, subscribing to topics, and
    /// managing the MQTT state.
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

        let minimq = Minimq {
            client: MqttClient {
                network_stack,
                clock,
                socket: None,
                session_state,
                connection_state: StateMachine::new(Context),
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
        for<'a> F: FnMut(&mut MqttClient<N, C, T>, &'a str, &[u8], &[Property<'a>]),
    {
        self.client.process()?;

        // If the connection is no longer active, reset the packet reader state and return. There's
        // nothing more we can do.
        if self.client.connection_state.state() != &States::Active {
            self.packet_reader.reset();
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
                let packet = ReceivedPacket::parse_message(&self.packet_reader)?;

                info!("Received {:?}", packet);

                let result = self.client.handle_packet(packet, &mut f);

                self.packet_reader.pop_packet()?;

                // If there was an error, return it now. Note that we ensure the packet is removed
                // from buffering after processing even in error conditions..
                result?;
            }
        }

        Ok(())
    }
}
