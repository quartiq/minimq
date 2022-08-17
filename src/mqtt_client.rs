use crate::{
    de::{
        deserialize::{ConnAck, ReceivedPacket},
        PacketReader,
    },
    network_manager::InterfaceHolder,
    ser::serialize,
    session_state::SessionState,
    will::Will,
    Error, Property, ProtocolError, QoS, Retain, MQTT_INSECURE_DEFAULT_PORT, {debug, error, info},
};

use embedded_nal::{IpAddr, SocketAddr, TcpClientStack};

use heapless::String;

use core::str::FromStr;

mod sm {

    use smlang::statemachine;

    statemachine! {
        transitions: {
            *Init + Update [ reconnect ] = Restart,
            Restart + Update [ connect_to_broker ] = Establishing,
            Establishing + ReceivedConnAck = Active,
            _ + Disconnect [ reconnect ] = Restart,
        },
        custom_guard_error: true,
    }
}

use sm::{Events, StateMachine, States};

struct ClientContext<
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
    const MSG_SIZE: usize,
    const MSG_COUNT: usize,
> {
    pub(crate) network: InterfaceHolder<TcpStack, MSG_SIZE>,
    session_state: SessionState<Clock, MSG_SIZE, MSG_COUNT>,
    will: Option<Will<MSG_SIZE>>,
}

impl<TcpStack, Clock, const MSG_SIZE: usize, const MSG_COUNT: usize> sm::StateMachineContext
    for ClientContext<TcpStack, Clock, MSG_SIZE, MSG_COUNT>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    type GuardError = crate::Error<TcpStack::Error>;

    fn reconnect(&mut self) -> Result<(), Self::GuardError> {
        self.network.allocate_socket()?;
        self.network.connect(self.session_state.broker)?;
        Ok(())
    }

    fn connect_to_broker(&mut self) -> Result<(), Self::GuardError> {
        if !self.network.tcp_connected()? {
            return Err(Error::NotReady);
        }

        let properties = [
            // Tell the broker our maximum packet size.
            Property::MaximumPacketSize(MSG_SIZE as u32),
            // The session does not expire.
            Property::SessionExpiryInterval(u32::MAX),
            Property::ReceiveMaximum(MSG_COUNT as u16),
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

        Ok(())
    }
}

pub struct MqttClient<
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
    const MSG_SIZE: usize,
    const MSG_COUNT: usize,
> {
    clock: Clock,
    sm: sm::StateMachine<ClientContext<TcpStack, Clock, MSG_SIZE, MSG_COUNT>>,
}

impl<
        TcpStack: TcpClientStack,
        Clock: embedded_time::Clock,
        const MSG_SIZE: usize,
        const MSG_COUNT: usize,
    > MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>
{
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

        self.sm.context_mut().will.replace(will);
        Ok(())
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
        if (self.sm.state() == &States::Active) || (self.sm.state() == &States::Establishing) {
            return Err(Error::NotReady);
        }

        self.sm
            .context_mut()
            .session_state
            .set_keepalive(interval_seconds);
        Ok(())
    }

    /// Set custom MQTT port to connect to.
    ///
    /// # Note
    /// This must be completed before connecting to a broker.
    ///
    /// # Args
    /// * `port` - The Port number to connect to.
    pub fn set_broker_port(&mut self, port: u16) -> Result<(), Error<TcpStack::Error>> {
        if self.sm.state() != &States::Restart {
            return Err(Error::NotReady);
        }

        self.sm.context_mut().session_state.broker.set_port(port);
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
        if self.sm.state() != &States::Active {
            return Err(Error::NotReady);
        }

        let ClientContext {
            network,
            session_state,
            ..
        } = self.sm.context_mut();

        // We can't subscribe if there's a pending write in the network.
        if network.has_pending_write() {
            return Err(Error::NotReady);
        }

        let packet_id = session_state.get_packet_identifier();

        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let packet = serialize::subscribe_message(&mut buffer, topic, packet_id, properties)?;

        network.write(packet).and_then(|_| {
            info!("Subscribing to `{}`: {}", topic, packet_id);
            session_state
                .pending_subscriptions
                .push(packet_id)
                .map_err(|_| Error::Unsupported)?;
            session_state.increment_packet_identifier();
            Ok(())
        })
    }

    ///
    /// # Returns
    /// True if any subscriptions are waiting for confirmation from the broker.
    pub fn subscriptions_pending(&self) -> bool {
        !self
            .sm
            .context()
            .session_state
            .pending_subscriptions
            .is_empty()
    }

    /// Determine if the client has established a connection with the broker.
    ///
    /// # Returns
    /// True if the client is connected to the broker.
    pub fn is_connected(&mut self) -> bool {
        self.sm.state() == &States::Active
    }

    /// Get the count of unacknowledged QoS 1 messages.
    ///
    /// # Returns
    /// Number of pending messages with QoS 1.
    pub fn pending_messages(&self, qos: QoS) -> usize {
        self.sm.context().session_state.pending_messages(qos)
    }

    /// Determine if the client is able to process QoS 1 publish requess.
    ///
    /// # Returns
    /// True if the client is able to service QoS 1 requests.
    pub fn can_publish(&self, qos: QoS) -> bool {
        // We cannot publish if there's a pending write in the network stack. That message must be
        // completed first.
        if self.sm.context().network.has_pending_write() {
            return false;
        }

        self.sm.context().session_state.can_publish(qos)
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
        // If we are not yet connected to the broker, we can't transmit a message.
        if !self.is_connected() {
            return Ok(());
        }

        if !self.can_publish(qos) {
            return Err(Error::NotReady);
        }

        debug!(
            "Publishing to `{}`: {:?} Props: {:?}",
            topic, data, properties
        );

        let ClientContext {
            session_state,
            network,
            ..
        } = self.sm.context_mut();

        // If QoS 0 the ID will be ignored
        let id = session_state.get_packet_identifier();

        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let packet =
            serialize::publish_message(&mut buffer, topic, data, qos, retain, id, properties)?;

        network.write(packet)?;
        session_state.increment_packet_identifier();

        if qos == QoS::AtLeastOnce {
            session_state.handle_publish(qos, id, packet);
        }

        Ok(())
    }

    fn handle_disconnection(&mut self) -> Result<(), Error<TcpStack::Error>> {
        match self.sm.process_event(Events::Disconnect) {
            Err(sm::Error::GuardFailed(error)) => return Err(error),
            other => other.unwrap(),
        };

        Ok(())
    }

    fn update(&mut self) -> Result<(), Error<TcpStack::Error>> {
        self.sm.process_event(Events::Update).ok();

        // Potentially update the state machine depending on the current socket connection status.
        if !self.sm.context_mut().network.tcp_connected()? && self.sm.state() != &States::Restart {
            self.handle_disconnection()?;
        }

        // Attempt to finish any pending packets.
        self.sm.context_mut().network.finish_write()?;

        self.handle_timers()?;

        Ok(())
    }

    fn handle_connection_acknowledge(
        &mut self,
        acknowledge: ConnAck,
    ) -> Result<(), Error<TcpStack::Error>> {
        if self.sm.state() != &States::Establishing {
            return Err(Error::Protocol(ProtocolError::Invalid));
        }

        if acknowledge.reason_code != 0 {
            return Err(Error::Failed(acknowledge.reason_code));
        }

        self.sm.process_event(Events::ReceivedConnAck).unwrap();

        let ClientContext {
            session_state,
            network,
            ..
        } = &mut self.sm.context_mut();

        let session_reset = !acknowledge.session_present && session_state.is_present();

        // Reset the session state upon connection with a broker that doesn't have a session state
        // saved for us.
        if !acknowledge.session_present {
            session_state.reset();
        }

        for property in acknowledge.properties {
            match property {
                Property::MaximumPacketSize(size) => {
                    session_state.maximum_packet_size.replace(size);
                }
                Property::AssignedClientIdentifier(id) => {
                    session_state.client_id =
                        String::from_str(id).or(Err(Error::ProvidedClientIdTooLong))?;
                }
                Property::ServerKeepAlive(keep_alive) => {
                    session_state.set_keepalive(keep_alive);
                }
                _prop => info!("Ignoring property: {:?}", _prop),
            };
        }

        // Now that we are connected, we have session state that will be persisted.
        session_state.register_connection(self.clock.try_now()?);

        // Replay QoS 1 messages
        for key in session_state.pending_publish_ordering.iter() {
            // If the network stack cannot send another message, do not attempt to send one.
            if network.has_pending_write() {
                break;
            }

            let message = session_state.pending_publish.get(key).unwrap();
            network.write(message)?;
        }

        if session_reset {
            Err(Error::SessionReset)
        } else {
            Ok(())
        }
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
        if self.sm.state() != &States::Active {
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
            }

            ReceivedPacket::PubAck(ack) => {
                // No matter the status code the message is considered acknowledged at this point
                self.sm
                    .context_mut()
                    .session_state
                    .handle_puback(ack.packet_identifier);
            }

            ReceivedPacket::SubAck(subscribe_acknowledge) => {
                let session_state = &mut self.sm.context_mut().session_state;
                match session_state
                    .pending_subscriptions
                    .iter()
                    .position(|v| *v == subscribe_acknowledge.packet_identifier)
                {
                    None => {
                        error!("Got bad suback: {:?}", subscribe_acknowledge);
                        return Err(Error::Protocol(ProtocolError::Invalid));
                    }
                    Some(index) => session_state.pending_subscriptions.swap_remove(index),
                };

                if subscribe_acknowledge.reason_code != 0 {
                    return Err(Error::Failed(subscribe_acknowledge.reason_code));
                }
            }

            ReceivedPacket::PingResp => {
                // Cancel the ping response timeout.
                self.sm.context_mut().session_state.register_ping_response();
            }

            _ => return Err(Error::Unsupported),
        };

        Ok(())
    }

    fn handle_timers(&mut self) -> Result<(), Error<TcpStack::Error>> {
        // If we are not connected, there's no session state to manage.
        if self.sm.state() != &States::Active {
            return Ok(());
        }

        // If there's a pending write, we can't send a ping no matter if it is due.
        if self.sm.context().network.has_pending_write() {
            return Ok(());
        }

        let now = self.clock.try_now()?;

        // Note: The ping timeout is set at this point so that it's running even if we fail
        // to write the ping message. This is intentional incase the underlying transport
        // mechanism has stalled. The ping timeout will then allow us to recover the
        // underlying TCP connection.
        match self.sm.context_mut().session_state.handle_ping(now) {
            Err(()) => self.handle_disconnection()?,

            Ok(true) => {
                let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];

                // Note: If we fail to serialize or write the packet, the ping timeout timer is
                // still running, so we will recover the TCP connection in the future.
                let packet = serialize::ping_req_message(&mut buffer)?;
                self.sm.context_mut().network.write(packet)?;
            }

            Ok(false) => {}
        }

        Ok(())
    }
}

/// The general structure for managing MQTT via Minimq.
pub struct Minimq<TcpStack, Clock, const MSG_SIZE: usize, const MSG_COUNT: usize>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    client: MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>,
    packet_reader: PacketReader<MSG_SIZE>,
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
            SocketAddr::new(broker, MQTT_INSECURE_DEFAULT_PORT),
            String::from_str(client_id).or(Err(Error::ProvidedClientIdTooLong))?,
        );

        let minimq = Minimq {
            client: MqttClient {
                clock,
                sm: StateMachine::new(ClientContext {
                    network: InterfaceHolder::new(network_stack),
                    session_state,
                    will: None,
                }),
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
        self.client.update()?;

        // If the connection is no longer active, reset the packet reader state and return. There's
        // nothing more we can do.
        if self.client.sm.state() != &States::Active
            && self.client.sm.state() != &States::Establishing
        {
            self.packet_reader.reset();
            return Ok(());
        }

        let mut buf: [u8; 1024] = [0; 1024];
        let received = self.client.sm.context_mut().network.read(&mut buf)?;
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
                    self.client.handle_disconnection()?;
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

    /// Directly access the MQTT client.
    pub fn client(&mut self) -> &mut MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT> {
        &mut self.client
    }
}
