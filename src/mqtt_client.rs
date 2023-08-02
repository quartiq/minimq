use crate::{
    de::{received_packet::ReceivedPacket, PacketReader},
    network_manager::InterfaceHolder,
    packets::{ConnAck, Connect, PingReq, Pub, PubAck, PubComp, PubRec, PubRel, SubAck, Subscribe},
    reason_codes::ReasonCode,
    session_state::SessionState,
    types::{Properties, TopicFilter, Utf8String},
    will::Will,
    Error, Property, ProtocolError, QoS, MQTT_INSECURE_DEFAULT_PORT, {debug, error, info, warn},
};

use embedded_time::{
    duration::{Extensions, Milliseconds, Seconds},
    Instant,
};

use embedded_nal::{IpAddr, SocketAddr, TcpClientStack};

use heapless::String;

use core::str::FromStr;

mod sm {

    use crate::{de::received_packet::ReceivedPacket, packets::ConnAck};
    use smlang::statemachine;

    statemachine! {
        transitions: {
            *Disconnected + TcpConnected = Restart,
            Restart + SentConnect = Establishing,
            Establishing + Connected(ConnAck<'a>) [ handle_connack ] = Active,

            Active + SendTimeout = Disconnected,
            _ + ProtocolError = Disconnected,

            Active + ControlPacket(ReceivedPacket<'a>) [handle_packet] = Active,
            Active + SentSubscribe(u16) [handle_subscription] = Active,

            _ + TcpDisconnect = Disconnected
        },
        custom_guard_error: true,
    }
}

use sm::{Events, StateMachine, States};

/// The default duration to wait for a ping response from the broker.
const PING_TIMEOUT: Seconds = Seconds(5);

struct ClientContext<
    'a,
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
> {
    session_state: SessionState<'a, TcpStack>,
    send_quota: u16,
    max_send_quota: u16,
    maximum_packet_size: Option<u32>,
    keep_alive_interval: Option<Milliseconds<u32>>,

    ping_timeout: Option<Instant<Clock>>,
    next_ping: Option<Instant<Clock>>,
    clock: Clock,
}

impl<'a, TcpStack, Clock> ClientContext<'a, TcpStack, Clock>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    pub fn new(clock: Clock, session_state: SessionState<'a, TcpStack>) {
        Self {
            session_state,
            send_quota: u16::MAX,
            max_send_quota: u16::MAX,
            clock,
            ping_timeout: None,
            next_ping: None,
            keep_alive_interval: Some(59_000.milliseconds()),
            maximum_packet_size: None,
        }
    }

    pub fn handle_suback(
        &mut self,
        subscribe_acknowledge: &SubAck<'_>,
    ) -> Result<(), Error<TcpStack::Error>> {
        match self
            .session_state
            .pending_subscriptions
            .iter()
            .position(|v| *v == subscribe_acknowledge.packet_identifier)
        {
            None => {
                error!("Got bad suback: {:?}", subscribe_acknowledge);
                return Err(Error::Protocol(ProtocolError::BadIdentifier));
            }
            Some(index) => self.session_state.pending_subscriptions.swap_remove(index),
        };

        for &code in subscribe_acknowledge.codes.iter() {
            ReasonCode::from(code).as_result()?;
        }

        Ok(())
    }

    /// Called whenever an active connection has been made with a broker.
    pub fn register_connection(&mut self) -> Result<(), Error<TcpStack::Error>> {
        self.active = true;
        self.ping_timeout = None;

        // The next ping should be sent out in half the keep-alive interval from now.
        if let Some(interval) = self.keep_alive_interval {
            self.next_ping.replace(self.clock.try_now()? + interval / 2);
        }

        // We just reconnected to the broker. Any of our pending publications will need to be
        // republished.
        for (_key, value) in self.pending_publish.iter_mut() {
            value.transmitted = false;
        }

        Ok(())
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

    /// Check if a pending ping is currently overdue.
    pub fn ping_is_overdue(&mut self) -> Result<bool, Error<TcpStack::Error>> {
        let now = self.clock.try_now()?;
        Ok(self
            .ping_timeout
            .map(|timeout| now > timeout)
            .unwrap_or(false))
    }

    /// Check if a ping is currently due for transmission.
    pub fn ping_is_due(&mut self) -> Result<bool, Error<TcpStack::Error>> {
        // If there's already a ping being transmitted, another can't be due.
        if self.ping_timeout.is_some() {
            return Ok(false);
        }

        let now = self.clock.try_now()?;

        Ok(self
            .keep_alive_interval
            .zip(self.next_ping)
            .map(|(keep_alive_interval, ping_deadline)| {
                // Update the next ping deadline if the ping is due.
                if now > ping_deadline {
                    // The next ping should be sent out in half the keep-alive interval from now.
                    self.next_ping.replace(now + keep_alive_interval / 2);
                    self.ping_timeout.replace(now + PING_TIMEOUT);
                }

                now > ping_deadline
            })
            .unwrap_or(false))
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
}

impl<'a, TcpStack, Clock> sm::StateMachineContext
    for ClientContext<'a, TcpStack, Clock>
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

    fn handle_packet(
        &mut self,
        packet: &ReceivedPacket<'_>,
    ) -> Result<(), Error<TcpStack::Error>> {
        match &packet {
            ReceivedPacket::SubAck(ack) => self.handle_suback(ack)?,
            ReceivedPacket::PingResp => self.register_ping_response(),
            ReceivedPacket::PubComp(comp) => {
                self.send_quota = self.send_quota.saturating_add(1).min(self.max_send_quota);
                self.session_state.handle_pubcomp(comp.packet_id)?;
            }
            ReceivedPacket::PubAck(ack) => {
                // No matter the status code the message is considered acknowledged at this point
                self.send_quota = self.send_quota.saturating_add(1).min(self.max_send_quota);
                self.session_state.handle_puback(ack.packet_identifier)?;
            }
            _ => return Err(Error::Protocol(ProtocolError::UnsupportedPacket)),
        }

        Ok(())
    }

    fn handle_connack(
        &mut self,
        acknowledge: &ConnAck<'_>,
    ) -> Result<(), Error<TcpStack::Error>> {
        acknowledge.reason_code.as_result()?;

        // Reset the session state upon connection with a broker that doesn't have a session state
        // saved for us.
        if !acknowledge.session_present {
            self.session_state.reset();
        }

        // If the server doesn't specify a send quota, assume it's 65535 as required by the spec
        // section 3.2.2.3.3 - this value is not part of the session state and is reset for each
        // connection.
        self.send_quota = u16::MAX;
        self.max_send_quota = u16::MAX;

        for property in acknowledge.properties.into_iter() {
            match property? {
                Property::MaximumPacketSize(size) => {
                    self.maximum_packet_size.replace(size);
                }
                Property::AssignedClientIdentifier(id) => {
                    self.session_state.client_id =
                        String::from_str(id.0).or(Err(Error::ProvidedClientIdTooLong))?;
                }
                Property::ServerKeepAlive(keep_alive) => {
                    self.set_keepalive(keep_alive);
                }
                Property::ReceiveMaximum(max) => {
                    self.send_quota = max;
                    self.max_send_quota = max;
                }
                _prop => info!("Ignoring property: {:?}", _prop),
            };
        }

        self.register_connection()?;

        Ok(())
    }
}

impl<T> From<sm::Error<Error<T>>> for Error<T> {
    fn from(error: sm::Error<Error<T>>) -> Error<T> {
        match error {
            sm::Error::GuardFailed(err) => err,
            sm::Error::InvalidEvent => Error::NotReady,
        }
    }
}

/// The client that can be used for interacting with the MQTT broker.
pub struct MqttClient<
    'buf,
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
> {
    sm: sm::StateMachine<ClientContext<'buf, TcpStack, Clock>>,
    network: InterfaceHolder<'buf, TcpStack>,
    will: Option<Will<'buf>>,
    broker: SocketAddr,
    max_packet_size: usize,
}

impl<
        'buf,
        TcpStack: TcpClientStack,
        Clock: embedded_time::Clock,
    > MqttClient<'buf, TcpStack, Clock>
{
    /// Specify the Will message to be sent if the client disconnects.
    ///
    /// # Args
    /// * `will` - The will to use.
    pub fn set_will(&mut self, will: Will<'buf>) -> Result<(), Error<TcpStack::Error>> {
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
    /// The subscription will not be completed immediately. Call
    /// `MqttClient::subscriptions_pending()` to check for subscriptions being completed.
    ///
    /// # Args
    /// * `topics` - A list of [`TopicFilter`]s to subscribe to.
    /// * `properties` - A list of properties to attach to the subscription request. May be empty.
    pub fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error<TcpStack::Error>> {
        if !self.is_connected() {
            return Err(Error::NotReady);
        }

        // We can't subscribe if there's a pending write in the network.
        if self.network.has_pending_write() {
            return Err(Error::NotReady);
        }

        let packet_id = self.sm.context_mut().session_state.get_packet_identifier();

        self.network.send_packet(&Subscribe {
            packet_id,
            properties: Properties::Slice(properties),
            topics,
        })?;

        self.sm.process_event(Events::SentSubscribe(packet_id))?;

        Ok(())
    }

    /// Check if any subscriptions have not yet been completed.
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
        self.sm
            .context()
            .session_state
            .pending_client_publications(qos)
    }

    /// Get the number of total message transactions that are in-progress.
    pub fn inflight_messages(&self) -> usize {
        let session_state = &self.sm.context().session_state;
        session_state.server_packet_ids().len()
            + session_state.pending_client_publications(QoS::AtLeastOnce)
            + session_state.pending_client_publications(QoS::ExactlyOnce)
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

        // If the server cannot handle another message with this quality of service, we can't send
        // one.
        if qos != QoS::AtMostOnce && self.sm.context().send_quota == 0 {
            return false;
        }

        // Otherwise, we can only send the message if we have the space for it in the session
        // state.
        self.sm.context().session_state.can_publish(qos)
    }

    /// Publish a message over MQTT.
    ///
    /// # Note
    /// If the client is not yet connected to the broker, the message will be silently ignored.
    ///
    /// # Args
    /// * `publish` - The publication to generate. See [Publication] for a builder pattern to
    /// generate a message.
    pub fn publish(&mut self, mut publish: Pub<'_>) -> Result<(), Error<TcpStack::Error>> {
        // If we are not yet connected to the broker, we can't transmit a message.
        if !self.is_connected() {
            return Ok(());
        }

        if !self.can_publish(publish.qos) {
            return Err(Error::NotReady);
        }

        publish.dup = false;

        // If QoS 0 the ID will be ignored
        publish.packet_id.take();
        if publish.qos > QoS::AtMostOnce {
            publish
                .packet_id
                .replace(self.sm.context_mut().session_state.get_packet_identifier());
        }

        let packet = self.network.send_packet(&publish)?;

        if let Some(id) = publish.packet_id {
            let context = self.sm.context_mut();
            context
                .session_state
                .handle_publish(publish.qos, id, packet)?;
            context.send_quota = context.send_quota.checked_sub(1).unwrap();
        }

        Ok(())
    }

    fn handle_restart(&mut self) -> Result<(), Error<TcpStack::Error>> {
        let properties = [
            // Tell the broker our maximum packet size.
            Property::MaximumPacketSize(self.max_packet_size as u32),
            // The session does not expire.
            Property::SessionExpiryInterval(u32::MAX),
            Property::ReceiveMaximum(self.pending_pubrel.capacity()),
        ];

        self.network.send_packet(&Connect {
            keep_alive: self.sm.context().session_state.keepalive_interval(),
            properties: Properties::Slice(&properties),
            client_id: Utf8String(self.sm.context().session_state.client_id.as_str()),
            will: self.will.as_ref(),
            clean_start: !self.sm.context().session_state.is_present(),
        })?;

        self.sm.process_event(Events::SentConnect)?;

        Ok(())
    }

    fn handle_active(&mut self) -> Result<(), Error<TcpStack::Error>> {
        if self.sm.context_mut().ping_is_overdue()? {
            warn!("Ping overdue. Trigging send timeout reset");
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

        if self.sm.context_mut().ping_is_due()? {
            // Note: If we fail to serialize or write the packet, the ping timeout timer is still
            // running, so we will recover the TCP connection in the future.
            self.network.send_packet(&PingReq {})?;
        }

        Ok(())
    }

    fn update(&mut self) -> Result<(), Error<TcpStack::Error>> {
        if self.network.socket_was_closed() {
            info!("Handling closed socket");
            self.sm.process_event(Events::TcpDisconnect).unwrap();
            self.network.allocate_socket()?;
        }

        // Attempt to finish any pending packets.
        self.network.finish_write()?;

        match *self.sm.state() {
            States::Disconnected => {
                if self.network.connect(self.broker)? {
                    info!("TCP socket connected");
                    self.sm.process_event(Events::TcpConnected)?;
                }
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
            &mut MqttClient<'buf, TcpStack, Clock>,
            &'a str,
            &[u8],
            &Properties<'a>,
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
                rec.reason.code().as_result().or_else(|e| {
                    let context = self.sm.context_mut();
                    context.send_quota = context
                        .send_quota
                        .saturating_add(1)
                        .min(context.max_send_quota);
                    context.session_state.remove_packet(rec.packet_id)?;
                    Err(e)
                })?;

                let pubrel = PubRel {
                    packet_id: rec.packet_id,
                    reason: Into::<ReasonCode>::into(
                        self.sm
                            .context_mut()
                            .session_state
                            .find_packet(rec.packet_id, QoS::ExactlyOnce),
                    )
                    .into(),
                };

                crate::info!("Sending: {:?}", pubrel);
                let packet = self.network.send_packet(&pubrel)?;

                self.sm
                    .context_mut()
                    .session_state
                    .handle_pubrec(rec.packet_id, packet)?;
            }

            ReceivedPacket::PubRel(rel) => {
                let session_state = &mut self.sm.context_mut().session_state;

                let reason = session_state.remove_server_packet_id(rel.packet_id);

                // Send a PubComp
                let pubcomp = PubComp {
                    packet_id: rel.packet_id,

                    // Note: Because we do not support ExactlyOnce, it's not possible for
                    // us to receive two packets with the same ID.
                    reason: reason.into(),
                };

                self.network.send_packet(&pubcomp)?;
            }

            ReceivedPacket::Publish(info) => {
                if &States::Active != self.sm.state() {
                    return Err(Error::NotReady);
                }

                // Handle transmitting any necessary acknowledges
                match info.qos {
                    QoS::AtMostOnce => {}
                    QoS::AtLeastOnce => {
                        // Note(uwnrap): There should always be a packet ID for QoS > AtMostOnce.
                        let packet_id = info.packet_id.unwrap();

                        // Reject the packet ID if it's currently in use for another publication.
                        let reason = if self
                            .sm
                            .context_mut()
                            .session_state
                            .server_packet_id_in_use(packet_id)
                        {
                            ReasonCode::PacketIdInUse
                        } else {
                            ReasonCode::Success
                        };

                        let puback = PubAck {
                            packet_identifier: info.packet_id.unwrap(),
                            reason: reason.into(),
                        };

                        self.network.send_packet(&puback)?;
                    }

                    QoS::ExactlyOnce => {
                        let session_state = &mut self.sm.context_mut().session_state;
                        // Note(uwnrap): There should always be a packet ID for QoS >
                        // AtMostOnce.
                        let packet_id = info.packet_id.unwrap();

                        // Check if the packet ID already exists before forwarding to app
                        // This procedure follows the MQTTv5 spec section 4.3.3 and 4.4
                        let duplicate = session_state.server_packet_id_in_use(packet_id);

                        let reason = session_state.push_server_packet_id(packet_id);

                        let pubrec = PubRec {
                            packet_id,
                            reason: reason.into(),
                        };

                        self.network.send_packet(&pubrec)?;

                        if duplicate || !pubrec.reason.code().success() {
                            return Ok(None);
                        }
                    }
                }

                // Provide the packet to the application for further processing.
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
///
/// # Note
/// To connect and maintain an MQTT connection, the `Minimq::poll()` method must be called
/// regularly.
pub struct Minimq<'buf, TcpStack, Clock>
where
    TcpStack: TcpClientStack,
    Clock: embedded_time::Clock,
{
    client: MqttClient<'buf, TcpStack, Clock>,
    packet_reader: PacketReader<'buf>,
}

impl<
        'buf,
        TcpStack: TcpClientStack,
        Clock: embedded_time::Clock,
    > Minimq<'buf, TcpStack, Clock>
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
        rx_buffer: &'buf mut [u8],
        tx_buffer: &'buf mut [u8],
        state_buffer: &'buf mut [u8],
    ) -> Result<Self, Error<TcpStack::Error>> {
        let session_state = SessionState::new(
            String::from_str(client_id).or(Err(Error::ProvidedClientIdTooLong))?,
            state_buffer,
        );

        let minimq = Minimq {
            client: MqttClient {
                sm: StateMachine::new(ClientContext::new(clock, session_state)),
                broker: SocketAddr::new(broker, MQTT_INSECURE_DEFAULT_PORT),
                will: None,
                network: InterfaceHolder::new(network_stack, tx_buffer),
                max_packet_size: rx_buffer.len(),
            },
            packet_reader: PacketReader::new(rx_buffer),
        };

        Ok(minimq)
    }

    /// Check the MQTT interface for available messages.
    ///
    /// # Note
    /// This method will processes as many MQTT control packets as possible until a PUBLISH message
    /// is received. The user should thus contintually call this function until it returns
    /// `Ok(None)`, as this is indicative that there is no further data to process.
    ///
    /// # Args
    /// * `f` - A closure to process any received messages. The closure should accept the client,
    /// topic, message, and list of proprties (in that order).
    ///
    /// # Returns
    /// Ok(Option<result>) - During normal operation, a <result> will optionally be returned to the
    /// user software if a value was returned from the `f` closure. If the closure was not
    /// executed, `None` is returned. Note that `None` may be returned even if MQTT packets were
    /// processed.
    ///
    /// Err(Error) if an MQTT-related error is encountered. Generally, Error::SessionReset is the
    /// only error expected during normal operation. In this case, the client has lost any previous
    /// subscriptions or session state.
    pub fn poll<F, T>(&mut self, mut f: F) -> Result<Option<T>, Error<TcpStack::Error>>
    where
        for<'a> F: FnMut(
            &mut MqttClient<'buf, TcpStack, Clock>,
            &'a str,
            &[u8],
            &Properties<'a>,
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

        // Read MQTT packets and process them until either:
        // 1. There are no available MQTT packets from the network
        // 2. A packet was processed that generates a result that needs to be propagated.
        loop {
            // Attempt to read an MQTT packet from the network.
            while !self.packet_reader.packet_available() {
                let buffer = match self.packet_reader.receive_buffer() {
                    Ok(buffer) => buffer,
                    Err(e) => {
                        warn!("Protocol Error reset: {e:?}");
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

            let packet = self.packet_reader.received_packet()?;
            info!("Received {:?}", packet);
            if let Some(result) = self.client.handle_packet(packet, &mut f)? {
                return Ok(Some(result));
            }
        }
    }

    /// Directly access the MQTT client.
    pub fn client(&mut self) -> &mut MqttClient<'buf, TcpStack, Clock> {
        &mut self.client
    }
}
