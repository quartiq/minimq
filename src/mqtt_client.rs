use crate::{
    de::{
        deserialize::{ConnAck, ReceivedPacket},
        PacketReader,
    },
    network_manager::InterfaceHolder,
    ser::serialize,
    session_state::SessionState,
    will::Will,
    Error, Property, ProtocolError, QoS, Retain, {debug, error, info},
};

use embedded_nal::{IpAddr, SocketAddr, TcpClientStack};

use heapless::String;

use core::str::FromStr;

mod sm {

    use smlang::statemachine;

    statemachine! {
        transitions: {
            *Restart + GotSocket = ConnectTransport,
            ConnectTransport + Connect = ConnectBroker,
            ConnectBroker + SentConnect = Establishing,
            ConnectBroker + Disconnect = Restart,
            Establishing + ReceivedConnAck = Active,
            Establishing + Disconnect = Restart,
            Active + Disconnect = Restart,
        }
    }

    pub struct Context;

    impl StateMachineContext for Context {}
}

use sm::{Context, Events, StateMachine, States};

/// The general structure for managing MQTT via Minimq.
pub struct Minimq<TcpStack, Clock, const MSG_SIZE: usize, const MSG_COUNT: usize>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    pub client: MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>,
    packet_reader: PacketReader<MSG_SIZE>,
}

/// A client for interacting with an MQTT Broker.
pub struct MqttClient<
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
    const MSG_SIZE: usize,
    const MSG_COUNT: usize,
> {
    pub(crate) network: InterfaceHolder<TcpStack, MSG_SIZE>,
    clock: Clock,
    session_state: SessionState<Clock, MSG_SIZE, MSG_COUNT>,
    connection_state: StateMachine<Context>,
    will: Option<Will<MSG_SIZE>>,
}

impl<TcpStack, Clock, const MSG_SIZE: usize, const MSG_COUNT: usize>
    MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    fn process(&mut self) -> Result<(), Error<TcpStack::Error>> {
        // Potentially update the state machine depending on the current socket connection status.
        if !self.network.tcp_connected()? {
            self.connection_state.process_event(Events::Disconnect).ok();
        } else {
            self.connection_state.process_event(Events::Connect).ok();
        }

        match self.connection_state.state() {
            // In the RESTART state, we need to reopen the TCP socket.
            &States::Restart => {
                self.network.allocate_socket()?;

                self.connection_state
                    .process_event(Events::GotSocket)
                    .unwrap();
            }

            // In the connect transport state, we need to connect our TCP socket to the broker.
            &States::ConnectTransport => {
                self.network
                    .connect(SocketAddr::new(self.session_state.broker, 1883))?;
            }

            // Next, connect to the broker via the MQTT protocol.
            &States::ConnectBroker => {
                let properties = [
                    // Tell the broker our maximum packet size.
                    Property::MaximumPacketSize(MSG_SIZE as u32),
                    // The session does not expire.
                    Property::SessionExpiryInterval(u32::MAX),
                ];

                let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
                let packet = serialize::connect_message(
                    &mut buffer,
                    self.session_state.client_id.as_str().as_bytes(),
                    self.session_state.keepalive_interval(),
                    &properties,
                    // Only perform a clean start if we do not have any session state.
                    !self.session_state.is_present(),
                    self.will.as_ref(),
                )?;

                info!("Sending CONNECT");
                self.network.write(packet)?;

                self.connection_state
                    .process_event(Events::SentConnect)
                    .unwrap();
            }

            &States::Establishing => {}

            _ => {}
        }

        self.handle_timers()?;

        Ok(())
    }

    /// Specify the Will message to be sent if the client disconnects.
    ///
    /// # Args
    /// * `topic` - The topic to send the message on
    /// * `data` - The message to transmit
    /// * `qos` - The quality of service at which to send the message.
    /// * `retained` - Specifies whether the will message should be retained by the broker.
    /// * `properties` - Any properties to send with the will message.
    pub fn set_will(
        &mut self,
        topic: &str,
        data: &[u8],
        qos: QoS,
        retained: Retain,
        properties: &[Property],
    ) -> Result<(), Error<TcpStack::Error>> {
        let mut will = Will::new(topic, data, properties)?;
        will.retained(retained);
        will.qos(qos);

        self.will.replace(will);
        Ok(())
    }

    fn reset(&mut self) {
        self.connection_state.process_event(Events::Disconnect).ok();
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
    pub fn set_keepalive_interval(
        &mut self,
        interval_seconds: u16,
    ) -> Result<(), Error<TcpStack::Error>> {
        if (self.connection_state.state() == &States::Active)
            || (self.connection_state.state() == &States::Establishing)
        {
            return Err(Error::NotReady);
        }

        self.session_state.set_keepalive(interval_seconds);
        Ok(())
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
    ) -> Result<(), Error<TcpStack::Error>> {
        if self.connection_state.state() != &States::Active {
            return Err(Error::NotReady);
        }

        let packet_id = self.session_state.get_packet_identifier();

        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let packet = serialize::subscribe_message(&mut buffer, topic, packet_id, properties)?;

        self.network.write(packet).and_then(|_| {
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
    pub fn is_connected(&mut self) -> bool {
        self.connection_state.state() == &States::Active
    }

    /// Get the count of unacknowledged QoS 1 messages.
    ///
    /// # Returns
    /// Number of pending messages with QoS 1.
    pub fn pending_messages(&self, qos: QoS) -> usize {
        self.session_state.pending_messages(qos)
    }

    /// Determine if the client is able to process QoS 1 publish requess.
    ///
    /// # Returns
    /// True if the client is able to service QoS 1 requests.
    pub fn can_publish(&self, qos: QoS) -> bool {
        self.session_state.can_publish(qos)
    }

    /// Publish a message over MQTT.
    ///
    /// # Note
    /// If the client is not yet connected to the broker, the message will be silently ignored.
    ///
    /// # Note
    /// Currently, QoS level 2 (exactly once) delivery is not supported.
    ///
    /// # Args
    /// * `topic` - The topic to publish the message to.
    /// * `data` - The data to transmit as the message contents.
    /// * `qos` - The desired quality-of-service level of the message. Must be QoS::AtMostOnce
    /// * `properties` - A list of properties to associate with the message being published. May be
    ///   empty.
    pub fn publish(
        &mut self,
        topic: &str,
        data: &[u8],
        qos: QoS,
        retain: Retain,
        properties: &[Property],
    ) -> Result<(), Error<TcpStack::Error>> {
        // TODO: QoS 2 support.
        assert!(qos != QoS::ExactlyOnce);

        // If we are not yet connected to the broker, we can't transmit a message.
        if self.is_connected() == false {
            return Ok(());
        }

        debug!(
            "Publishing to `{}`: {:?} Props: {:?}",
            topic, data, properties
        );

        // If QoS 0 the ID will be ignored
        let id = self.session_state.get_packet_identifier();

        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let packet =
            serialize::publish_message(&mut buffer, topic, data, qos, retain, id, properties)?;

        self.network.write(packet)?;
        self.session_state.increment_packet_identifier();

        if qos == QoS::AtLeastOnce {
            self.session_state.handle_publish(qos, id, packet);
        }

        Ok(())
    }

    /// Finish sending last packet if the previous operation returned Error::PartialWrite
    ///
    /// # Note
    /// If this function is not called after partial write until the packet is
    /// fully sent, the packets will most likely seem malformed to the receiving
    /// party and they will close the connection.
    pub fn finish_write(&mut self) -> Result<(), Error<TcpStack::Error>> {
        self.network.finish_write()
    }

    fn handle_connection_acknowledge(
        &mut self,
        acknowledge: ConnAck,
    ) -> Result<(), Error<TcpStack::Error>> {
        if self.connection_state.state() != &States::Establishing {
            return Err(Error::Protocol(ProtocolError::Invalid));
        }

        let mut result = Ok(());

        if acknowledge.reason_code != 0 {
            return Err(Error::Failed(acknowledge.reason_code));
        }

        if !acknowledge.session_present {
            if self.session_state.is_present() {
                result = Err(Error::SessionReset);
            }

            // Reset the session state upon connection with a broker that doesn't have a
            // session state saved for us.
            self.session_state.reset();
        }

        self.connection_state
            .process_event(Events::ReceivedConnAck)
            .unwrap();

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
                    self.session_state.set_keepalive(keep_alive);
                }
                _prop => info!("Ignoring property: {:?}", _prop),
            };
        }

        // Now that we are connected, we have session state that will be persisted.
        self.session_state
            .register_connection(self.clock.try_now()?);

        // Replay QoS 1 messages
        for key in self.session_state.pending_publish_ordering.iter() {
            let message = self.session_state.pending_publish.get(&key).unwrap();
            self.network.write(message)?;
        }

        result
    }

    fn handle_packet<'a, F>(
        &mut self,
        packet: ReceivedPacket<'a>,
        f: &mut F,
    ) -> Result<(), Error<TcpStack::Error>>
    where
        F: FnMut(
            &mut MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>,
            &'a str,
            &[u8],
            &[Property<'a>],
        ),
    {
        // ConnAck packets are received outside of the connection state.
        if let ReceivedPacket::ConnAck(ack) = packet {
            return self.handle_connection_acknowledge(ack);
        }

        // All other packets must be received in the active state.
        if !self.is_connected() {
            error!(
                "Received invalid packet outside of connected state: {:?}",
                packet
            );
            return Err(Error::Protocol(ProtocolError::Invalid));
        }

        match packet {
            ReceivedPacket::Publish(info) => {
                // Call a handler function to deal with the received data.
                f(self, info.topic, info.payload, &info.properties);

                Ok(())
            }

            ReceivedPacket::PubAck(ack) => {
                // No matter the status code the message is considered acknowledged at this point
                self.session_state.handle_puback(ack.packet_identifier);

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
                self.session_state.register_ping_response();
                Ok(())
            }

            _ => Err(Error::Unsupported),
        }
    }

    fn handle_timers(&mut self) -> Result<(), Error<TcpStack::Error>> {
        // If we are not connected, there's no session state to manage.
        if self.connection_state.state() != &States::Active {
            return Ok(());
        }

        let now = self.clock.try_now()?;

        // Note: The ping timeout is set at this point so that it's running even if we fail
        // to write the ping message. This is intentional incase the underlying transport
        // mechanism has stalled. The ping timeout will then allow us to recover the
        // underlying TCP connection.
        match self.session_state.handle_ping(now) {
            Err(()) => {
                self.connection_state.process_event(Events::Disconnect).ok();
            }

            Ok(true) => {
                let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];

                // Note: If we fail to serialize or write the packet, the ping timeout timer is
                // still running, so we will recover the TCP connection in the future.
                let packet = serialize::ping_req_message(&mut buffer)?;
                self.network.write(packet)?;
            }

            Ok(false) => {}
        }

        Ok(())
    }
}

impl<
        TcpStack: TcpClientStack,
        Clock: embedded_time::Clock,
        const MSG_SIZE: usize,
        const MSG_COUNT: usize,
    > Minimq<TcpStack, Clock, MSG_SIZE, MSG_COUNT>
{
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
        network_stack: TcpStack,
        clock: Clock,
    ) -> Result<Self, Error<TcpStack::Error>> {
        let session_state = SessionState::new(
            broker,
            String::from_str(client_id).or(Err(Error::ProvidedClientIdTooLong))?,
        );

        let minimq = Minimq {
            client: MqttClient {
                network: InterfaceHolder::new(network_stack),
                clock,
                session_state,
                connection_state: StateMachine::new(Context),
                will: None,
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
    pub fn poll<F>(&mut self, mut f: F) -> Result<(), Error<TcpStack::Error>>
    where
        for<'a> F: FnMut(
            &mut MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>,
            &'a str,
            &[u8],
            &[Property<'a>],
        ),
    {
        self.client.process()?;

        // If the connection is no longer active, reset the packet reader state and return. There's
        // nothing more we can do.
        if self.client.connection_state.state() != &States::Active
            && self.client.connection_state.state() != &States::Establishing
        {
            self.packet_reader.reset();
            return Ok(());
        }

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.client.network.read(&mut buf)?;
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
