use super::drive::fill_packet_reader;
use super::*;
use embedded_io_async::Error as _;

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    /// Establish or resume the MQTT session on a newly supplied transport.
    ///
    /// The session takes ownership of `io` for the lifetime of the connected session and drops it
    /// again on disconnect or connection failure. If cancelled during the handshake, `io` is
    /// dropped and a later `connect()` restarts from clean transport-local state.
    pub async fn connect(&mut self, mut io: IO) -> Result<ConnectEvent, Error<IO::Error>> {
        if self.connection.is_some() {
            return Err(Error::NotReady);
        }
        self.packet_reader.reset();
        self.runtime.reset_transport();
        let result = self.connect_handshake(&mut io).await;
        if result.is_ok() {
            self.connection = Some(io);
        }
        result
    }

    async fn connect_handshake(
        &mut self,
        connection: &mut IO,
    ) -> Result<ConnectEvent, Error<IO::Error>> {
        let client_id = self.client_id.clone();
        let properties = [
            Property::MaximumPacketSize(self.packet_reader.buffer.len() as u32),
            Property::SessionExpiryInterval(self.session_expiry_interval),
            Property::ReceiveMaximum(self.data.pending_server_packet_ids.capacity() as u16),
        ];
        let will = self.will.clone();
        let keepalive = self.runtime.keepalive_interval.as_secs() as u16;
        let clean_start = !self.data.session_present;
        let auth = self.auth;
        debug!(
            "Sending CONNECT: client_id={} clean_start={} keepalive_s={} session_expiry={} receive_max={} rx_max_packet_size={}",
            client_id,
            clean_start,
            keepalive,
            self.session_expiry_interval,
            self.data.pending_server_packet_ids.capacity(),
            self.packet_reader.buffer.len()
        );

        {
            let buffer = self.data.outbound.scratch_space();
            write_packet(
                buffer,
                connection,
                &Connect {
                    keepalive,
                    properties: Properties::Slice(&properties),
                    client_id: Utf8String(client_id.as_str()),
                    auth,
                    will,
                    clean_start,
                },
            )
            .await?;
        }

        self.runtime.next_ping = None;
        self.runtime.ping_timeout = None;

        if let Err(err) = fill_packet_reader(&mut self.packet_reader, connection).await {
            match &err {
                Error::Transport(err) => warn!("Transport read failed: {:?}", err.kind()),
                Error::Disconnected => warn!("Transport returned EOF during CONNECT"),
                _ => {}
            }
            self.handle_disconnect();
            return Err(err);
        }

        let packet = match self.packet_reader.received_packet() {
            Ok(packet) => packet,
            Err(err) => {
                warn!("Failed to decode inbound packet: {:?}", err);
                self.handle_disconnect();
                return Err(err.into());
            }
        };
        let ack = match packet {
            ReceivedPacket::ConnAck(ack) => ack,
            ReceivedPacket::Disconnect(_) => {
                info!("Received broker DISCONNECT during CONNECT");
                self.handle_disconnect();
                return Err(Error::Disconnected);
            }
            _ => {
                self.handle_disconnect();
                return Err(Error::Protocol(ProtocolError::UnexpectedPacket));
            }
        };

        if let Err(err) = ack.reason_code.as_result() {
            warn!("Broker rejected CONNECT with reason {:?}", ack.reason_code);
            self.handle_disconnect();
            return Err(Error::Protocol(err));
        }

        let resumed = ack.session_present;
        if !resumed {
            debug!("Broker started a fresh session; resetting local session state");
            self.data.reset();
        }

        let local_quota = self.data.outbound.max_inflight();
        let mut send_quota = local_quota;
        let mut max_send_quota = local_quota;
        let mut max_qos = None;
        let mut maximum_packet_size = None;
        let mut keepalive_interval = self.runtime.keepalive_interval;
        let mut assigned_client_id = None;

        for property in ack.properties.into_iter() {
            match match property {
                Ok(property) => property,
                Err(err) => {
                    self.handle_disconnect();
                    return Err(Error::Protocol(err));
                }
            } {
                Property::MaximumPacketSize(size) => maximum_packet_size = Some(size),
                Property::AssignedClientIdentifier(id) => {
                    assigned_client_id = Some(match String::try_from(id.0) {
                        Ok(client_id) => client_id,
                        Err(_) => {
                            self.handle_disconnect();
                            return Err(Error::Protocol(ProtocolError::ProvidedClientIdTooLong));
                        }
                    });
                }
                Property::ServerKeepAlive(keepalive) => {
                    keepalive_interval = Duration::from_secs(keepalive as u64);
                }
                Property::ReceiveMaximum(max) => {
                    if max == 0 {
                        self.handle_disconnect();
                        return Err(Error::Protocol(ProtocolError::InvalidProperty));
                    }
                    send_quota = max.min(local_quota);
                    max_send_quota = max.min(local_quota);
                }
                Property::MaximumQoS(max) => {
                    max_qos = Some(match QoS::try_from(max) {
                        Ok(qos) => qos,
                        Err(_) => {
                            self.handle_disconnect();
                            return Err(Error::Protocol(ProtocolError::WrongQos));
                        }
                    });
                }
                _ => {}
            }
        }

        self.runtime.session_resumed = resumed;
        self.runtime.keepalive_interval = keepalive_interval;
        self.runtime.send_quota = send_quota;
        self.runtime.max_send_quota = max_send_quota;
        self.runtime.max_qos = max_qos;
        self.runtime.maximum_packet_size = maximum_packet_size;
        if let Some(assigned_client_id) = assigned_client_id {
            self.client_id = assigned_client_id;
        }

        debug!(
            "Activated session state resumed={} send_quota={}/{} max_qos={:?} broker_max_packet_size={:?}",
            resumed,
            self.runtime.send_quota,
            self.runtime.max_send_quota,
            self.runtime.max_qos,
            self.runtime.maximum_packet_size
        );

        self.data.register_connected();
        self.runtime.note_outbound_activity(Instant::now());
        self.runtime.ping_timeout = None;
        if resumed {
            info!("Connected and resumed existing broker session");
            Ok(ConnectEvent::Reconnected)
        } else {
            info!("Connected with a fresh broker session");
            Ok(ConnectEvent::Connected)
        }
    }
}
