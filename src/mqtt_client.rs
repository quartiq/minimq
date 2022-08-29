use crate::{
    de::{packets::ReceivedPacket, PacketReader},
    network_manager::InterfaceHolder,
    packets::{ConnAck, Connect, PingReq, Pub, PubRel, SubAck, Subscribe},
    ser::serialize,
    session_state::SessionState,
    types::{Properties, SubscriptionOptions, Utf8String},
    will::Will,
    Error, Property, ProtocolError, QoS, Retain, MQTT_INSECURE_DEFAULT_PORT, {debug, error, info},
};

use embedded_nal::{IpAddr, SocketAddr, TcpClientStack};

use heapless::{String, Vec};

use core::str::FromStr;

mod sm {

    use crate::{de::packets::ReceivedPacket, packets::ConnAck};
    use smlang::statemachine;

    statemachine! {
        transitions: {
            *Disconnected + Reallocated = Restart,
            Restart + SentConnect = Establishing,
            Establishing + Connected(ConnAck<'a>) [ handle_connack ] = Active,

            Active + SendTimeout = Disconnected,
            _ + ProtocolError = Disconnected,

            Active + ControlPacket(ReceivedPacket<'a>) [handle_packet] = Active,
            Active + SentSubscribe(u16) [handle_subscription] = Active,

            Establishing + TcpDisconnect = Disconnected,
            Active + TcpDisconnect = Disconnected,
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
    session_state: SessionState<TcpStack, Clock, MSG_SIZE, MSG_COUNT>,
}

impl<TcpStack, Clock, const MSG_SIZE: usize, const MSG_COUNT: usize>
    ClientContext<TcpStack, Clock, MSG_SIZE, MSG_COUNT>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    pub fn handle_suback<'a>(
        &mut self,
        subscribe_acknowledge: &SubAck<'a>,
    ) -> Result<(), Error<TcpStack::Error>> {
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

        if subscribe_acknowledge.code() != 0 {
            return Err(Error::Failed(subscribe_acknowledge.code()));
        }

        Ok(())
    }
}

impl<TcpStack, Clock, const MSG_SIZE: usize, const MSG_COUNT: usize> sm::StateMachineContext
    for ClientContext<TcpStack, Clock, MSG_SIZE, MSG_COUNT>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    type GuardError = crate::Error<TcpStack::Error>;

    fn handle_subscription(&mut self, id: &u16) -> Result<(), Error<TcpStack::Error>> {
        self.session_state
            .pending_subscriptions
            .push(*id)
            .map_err(|_| Error::Unsupported)?;
        Ok(())
    }

    fn handle_packet<'a>(
        &mut self,
        packet: &ReceivedPacket<'a>,
    ) -> Result<(), Error<TcpStack::Error>> {
        match &packet {
            ReceivedPacket::SubAck(ack) => self.handle_suback(ack)?,
            ReceivedPacket::PingResp => self.session_state.register_ping_response(),
            ReceivedPacket::PubComp(comp) => self.session_state.handle_pubcomp(comp.packet_id)?,
            ReceivedPacket::PubAck(ack) => {
                // No matter the status code the message is considered acknowledged at this point
                self.session_state.handle_puback(ack.packet_identifier)?;
            }
            _ => return Err(Error::Protocol(ProtocolError::Invalid)),
        }

        Ok(())
    }

    fn handle_connack<'a>(
        &mut self,
        acknowledge: &ConnAck<'a>,
    ) -> Result<(), Error<TcpStack::Error>> {
        if acknowledge.reason_code != 0 {
            return Err(Error::Failed(acknowledge.reason_code));
        }

        // Reset the session state upon connection with a broker that doesn't have a session state
        // saved for us.
        if !acknowledge.session_present {
            self.session_state.reset();
        }

        for property in &acknowledge.properties {
            match property {
                Property::MaximumPacketSize(size) => {
                    self.session_state.maximum_packet_size.replace(*size);
                }
                Property::AssignedClientIdentifier(id) => {
                    self.session_state.client_id =
                        String::from_str(id.0).or(Err(Error::ProvidedClientIdTooLong))?;
                }
                Property::ServerKeepAlive(keep_alive) => {
                    self.session_state.set_keepalive(*keep_alive);
                }
                _prop => info!("Ignoring property: {:?}", _prop),
            };
        }

        self.session_state.register_connection()?;

        Ok(())
    }
}

impl<T> From<sm::Error<Error<T>>> for Error<T> {
    fn from(error: sm::Error<Error<T>>) -> Error<T> {
        match error {
            sm::Error::GuardFailed(err) => err,
            sm::Error::InvalidEvent => Error::Protocol(ProtocolError::Invalid),
        }
    }
}

pub struct MqttClient<
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
    const MSG_SIZE: usize,
    const MSG_COUNT: usize,
> {
    sm: sm::StateMachine<ClientContext<TcpStack, Clock, MSG_SIZE, MSG_COUNT>>,
    network: InterfaceHolder<TcpStack, MSG_SIZE>,
    will: Option<Will<MSG_SIZE>>,
    broker: SocketAddr,
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

        self.will.replace(will);
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
        if self.sm.state() != &States::Disconnected {
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
        if self.sm.state() != &States::Disconnected {
            return Err(Error::NotReady);
        }

        self.broker.set_port(port);
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
        if !self.is_connected() {
            return Err(Error::NotReady);
        }

        // We can't subscribe if there's a pending write in the network.
        if self.network.has_pending_write() {
            return Err(Error::NotReady);
        }

        let packet_id = self.sm.context_mut().session_state.get_packet_identifier();

        let subscribe = Subscribe {
            packet_id,
            properties: Properties(properties),
            topics: &[(Utf8String(topic), SubscriptionOptions {})],
        };

        info!("Sending: {:?}", subscribe);
        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let packet = serialize::serialize_control_packet(&mut buffer, subscribe)?;

        self.network.write(packet)?;
        self.sm.process_event(Events::SentSubscribe(packet_id))?;

        Ok(())
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
        matches!(self.sm.state(), &States::Active)
    }

    /// Get the count of unacknowledged messages at the requested QoS.
    ///
    /// # Args
    /// * `qos` - The QoS to check messages of.
    ///
    /// # Returns
    /// Number of pending messages with the specified QoS.
    pub fn pending_messages(&self, qos: QoS) -> usize {
        self.sm.context().session_state.pending_messages(qos)
    }

    /// Determine if the client is able to process publish requests.
    ///
    /// # Args
    /// * `qos` - The QoS level to check publish capabilities of.
    ///
    /// # Returns
    /// True if the client is able to publish at the requested QoS.
    pub fn can_publish(&self, qos: QoS) -> bool {
        // We cannot publish if there's a pending write in the network stack. That message must be
        // completed first.
        if self.network.has_pending_write() {
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

        info!(
            "Publishing to `{}`: {:?} Props: {:?}",
            topic, data, properties
        );

        // If QoS 0 the ID will be ignored
        let id = self.sm.context_mut().session_state.get_packet_identifier();

        let publish = Pub {
            topic: Utf8String(topic),
            properties: Vec::from_slice(properties).map_err(|_| Error::TooManyProperties)?,
            packet_id: Some(id),
            payload: data,
            retain,
            qos,
            dup: false,
        };

        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let packet = serialize::serialize_control_packet(&mut buffer, publish)?;

        self.network.write(packet)?;

        // TODO: Generate event.
        self.sm
            .context_mut()
            .session_state
            .handle_publish(qos, id, packet)?;

        Ok(())
    }

    fn handle_restart(&mut self) -> Result<(), Error<TcpStack::Error>> {
        if !self.network.tcp_connected()? {
            return Ok(());
        }

        let properties = [
            // Tell the broker our maximum packet size.
            Property::MaximumPacketSize(MSG_SIZE as u32),
            // The session does not expire.
            Property::SessionExpiryInterval(u32::MAX),
            Property::ReceiveMaximum(MSG_COUNT as u16),
        ];

        let connect = Connect {
            keep_alive: self.sm.context().session_state.keepalive_interval(),
            properties: Properties(&properties),
            client_id: Utf8String(self.sm.context().session_state.client_id.as_str()),
            will: self.will.as_ref(),
            clean_start: !self.sm.context().session_state.is_present(),
        };

        info!("Sending {:?}", connect);
        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let packet = serialize::serialize_control_packet(&mut buffer, connect)?;

        self.network.write(packet)?;

        self.sm.process_event(Events::SentConnect)?;

        Ok(())
    }

    fn handle_active(&mut self) -> Result<(), Error<TcpStack::Error>> {
        if self.sm.context_mut().session_state.ping_is_overdue()? {
            self.sm.process_event(Events::SendTimeout).unwrap();
        }

        while !self.network.has_pending_write() {
            if let Some(msg) = self
                .sm
                .context_mut()
                .session_state
                .next_pending_republication()
            {
                self.network.write(msg)?;
            } else {
                break;
            }
        }

        // If there's a pending write, we can't send a ping no matter if it is due. This is
        // intentionally done before checking if a ping is due, since we wouldn't be able to send
        // the ping otherwise.
        if self.network.has_pending_write() {
            return Ok(());
        }

        if self.sm.context_mut().session_state.ping_is_due()? {
            // Note: If we fail to serialize or write the packet, the ping timeout timer is
            // still running, so we will recover the TCP connection in the future.
            let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
            let packet = serialize::serialize_control_packet(&mut buffer, PingReq {})?;
            self.network.write(packet)?;
        }

        Ok(())
    }

    fn update(&mut self) -> Result<(), Error<TcpStack::Error>> {
        // Potentially update the state machine depending on the current socket connection status.
        let tcp_connected = self.network.tcp_connected()?;

        if !tcp_connected {
            self.sm.process_event(Events::TcpDisconnect).ok();
        }

        // Attempt to finish any pending packets.
        self.network.finish_write()?;

        match *self.sm.state() {
            States::Disconnected => {
                self.network.allocate_socket()?;
                self.network.connect(self.broker)?;
                self.sm.process_event(Events::Reallocated)?;
            }
            States::Restart => self.handle_restart()?,
            States::Active => self.handle_active()?,
            States::Establishing => {}
        };

        Ok(())
    }

    fn handle_packet<'a, F, T>(
        &mut self,
        control_packet: ReceivedPacket<'a>,
        f: &mut F,
    ) -> Result<Option<T>, Error<TcpStack::Error>>
    where
        F: FnMut(
            &mut MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>,
            &'a str,
            &[u8],
            &[Property<'a>],
        ) -> T,
    {
        match control_packet {
            ReceivedPacket::ConnAck(ack) => {
                self.sm.process_event(Events::Connected(ack))?;

                return if self.sm.context_mut().session_state.was_reset() {
                    Err(Error::SessionReset)
                } else {
                    Ok(None)
                };
            }

            ReceivedPacket::PubRec(rec) => {
                if rec.reason.code() >= 0x80 {
                    self.sm
                        .context_mut()
                        .session_state
                        .remove_packet(rec.packet_id)?;
                    return Err(Error::Protocol(ProtocolError::Rejected(rec.reason.code())));
                }

                let pubrel = PubRel {
                    packet_id: rec.packet_id,
                    code: 0,
                    properties: Properties(&[]),
                };
                info!("Sending {:?}", pubrel);

                let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
                let packet = serialize::serialize_control_packet(&mut buffer, pubrel)?;
                self.network.write(packet)?;

                // TODO: Utilize errors to generate reason codes.
                self.sm
                    .context_mut()
                    .session_state
                    .handle_pubrec(rec.packet_id, packet)?;
            }

            ReceivedPacket::Publish(info) => {
                if &States::Active != self.sm.state() {
                    return Err(Error::Protocol(ProtocolError::Invalid));
                }

                return Ok(Some(f(self, info.topic.0, info.payload, &info.properties)));
            }

            _ => {
                self.sm
                    .process_event(Events::ControlPacket(control_packet))?;
            }
        }

        Ok(None)
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
            clock,
            String::from_str(client_id).or(Err(Error::ProvidedClientIdTooLong))?,
        );

        let minimq = Minimq {
            client: MqttClient {
                sm: StateMachine::new(ClientContext { session_state }),
                broker: SocketAddr::new(broker, MQTT_INSECURE_DEFAULT_PORT),
                will: None,
                network: InterfaceHolder::new(network_stack),
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
    pub fn poll<F, T>(&mut self, mut f: F) -> Result<Option<T>, Error<TcpStack::Error>>
    where
        for<'a> F: FnMut(
            &mut MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT>,
            &'a str,
            &[u8],
            &[Property<'a>],
        ) -> T,
    {
        self.client.update()?;

        // If the connection is no longer active, reset the packet reader state and return. There's
        // nothing more we can do.
        if self.client.sm.state() != &States::Active
            && self.client.sm.state() != &States::Establishing
        {
            self.packet_reader.reset();
            return Ok(None);
        }

        // Attempt to read an MQTT packet from the network.
        while !self.packet_reader.packet_available() {
            let buffer = match self.packet_reader.receive_buffer() {
                Ok(buffer) => buffer,
                Err(e) => {
                    self.client.sm.process_event(Events::ProtocolError).unwrap();
                    self.packet_reader.reset();
                    return Err(Error::Protocol(e));
                }
            };

            let received = self.client.network.read(buffer)?;
            self.packet_reader.commit(received);

            if received > 0 {
                debug!("Received {} bytes", received);
            } else {
                return Ok(None);
            }
        }

        let result = {
            let packet = self.packet_reader.received_packet()?;
            info!("Received {:?}", packet);
            self.client.handle_packet(packet, &mut f)
        };

        // We have now processed the packet. Remove it from the reader.
        self.packet_reader.reset();

        result
    }

    /// Directly access the MQTT client.
    pub fn client(&mut self) -> &mut MqttClient<TcpStack, Clock, MSG_SIZE, MSG_COUNT> {
        &mut self.client
    }
}
