use embassy_time::{Duration, Instant};
use embedded_io_async::Error as _;
use heapless::String;

use crate::config::CLIENT_ID_CAPACITY;
use crate::de::ReceivedPacket;
use crate::mqtt_client::ConnectEvent;
use crate::mqtt_client::outbound::write_all;
use crate::packets::Connect;
use crate::properties::Properties;
use crate::ser::MqttSerializer;
use crate::wire::Utf8String;
use crate::{Error, PeerError, Property, QoS, debug, info, warn};

use super::{Connection, Io, Session, drive::fill_packet_reader};

impl<'buf> Session<'buf> {
    /// Establish or resume the MQTT session on a newly supplied transport.
    ///
    /// On success the returned [`Connection`] takes ownership of `io` for the lifetime of the
    /// connected session and borrows this session; all network operations are driven through it.
    /// On handshake failure (or cancellation) `io` is dropped and the session is left disconnected.
    ///
    /// Each attempt resets the packet reader and keepalive timers and prepares in-flight
    /// packets for replay, regardless of how the previous connection ended. CONNACK decides
    /// whether session state survives. Dropping a handle sends no `DISCONNECT`.
    /// MQTT 5 and persistent MQTT 3.1.1
    /// sessions resume broker state when available ([`ConnectEvent::Reconnected`]); otherwise
    /// a fresh session is started ([`ConnectEvent::Connected`]). MQTT 3.1.1 clean sessions
    /// always start fresh.
    pub async fn connect<IO: Io>(
        &mut self,
        mut io: IO,
    ) -> Result<Connection<'_, 'buf, IO>, Error<IO::Error>> {
        if let crate::Protocol::V3_1_1 { clean_session } = self.protocol
            && (self.session_expiry_interval.is_some()
                || self.downgrade_qos
                || self
                    .will
                    .as_ref()
                    .is_some_and(|will| !will.properties.is_empty())
                || (!clean_session && self.client_id.is_empty()))
        {
            return Err(Error::InvalidRequest);
        }
        // This is the sole reset boundary, including after a cancelled handshake
        // or a dropped connection that never reported an error.
        self.packet_reader.reset();
        self.runtime.next_ping = None;
        self.runtime.ping_timeout = None;
        self.data.outbound.arm_replay();
        let event = self.connect_handshake(&mut io).await?;
        Ok(Connection {
            session: self,
            io,
            event,
            live: true,
            flush_state: super::FlushState::Clean,
        })
    }

    async fn connect_handshake<IO: Io>(
        &mut self,
        connection: &mut IO,
    ) -> Result<ConnectEvent, Error<IO::Error>> {
        let client_id = self.client_id.clone();
        let properties = [
            Property::MaximumPacketSize(self.packet_reader.buffer.len() as u32),
            Property::SessionExpiryInterval(self.session_expiry_interval.unwrap_or(0)),
            Property::ReceiveMaximum(self.data.pending_server_packet_ids.capacity() as u16),
        ];
        let will = self.will.clone();
        let keepalive = self.keepalive_interval.as_secs() as u16;
        let clean_start = match self.protocol {
            crate::Protocol::V5 => !self.data.session_present,
            crate::Protocol::V3_1_1 { clean_session } => clean_session,
        };
        let auth = self.auth;
        debug!(
            "Sending CONNECT: client_id={=str} clean_start={=bool} keepalive_s={=u16} session_expiry={=u32} receive_max={=u16} rx_max_packet_size={=usize}",
            client_id,
            clean_start,
            keepalive,
            self.session_expiry_interval.unwrap_or(0),
            self.data.pending_server_packet_ids.capacity() as u16,
            self.packet_reader.buffer.len()
        );

        let connect_workspace = {
            let buffer = self.data.outbound.scratch_space();
            let (offset, packet) = MqttSerializer::encode_with_offset(
                buffer,
                &Connect {
                    protocol: self.protocol,
                    keepalive,
                    properties: Properties::from_slice(&properties),
                    client_id: Utf8String(client_id.as_str()),
                    auth,
                    will,
                    clean_start,
                },
            )?;
            let workspace = offset + packet.len();
            write_all(connection, packet).await?;
            connection.flush().await.map_err(Error::Transport)?;
            workspace
        };
        self.data
            .outbound
            .set_connect_workspace(connect_workspace)?;

        if let Err(err) = fill_packet_reader(&mut self.packet_reader, connection).await {
            match &err {
                Error::Transport(err) => warn!("Transport read failed: {}", err.kind()),
                Error::Disconnected => warn!("Transport returned EOF during CONNECT"),
                _ => {}
            }
            return Err(err);
        }

        let packet = match self.packet_reader.received_packet(self.protocol) {
            Ok(packet) => packet,
            Err(err) => {
                warn!("Failed to decode inbound packet: {}", err);
                return Err(err.into());
            }
        };
        let ack = match packet {
            ReceivedPacket::ConnAck(ack) => ack,
            ReceivedPacket::Disconnect(disconnect) => {
                info!(
                    "Received broker DISCONNECT during CONNECT reason={}",
                    disconnect.reason_code()
                );
                return Err(Error::Disconnected);
            }
            _ => return Err(Error::Peer(PeerError::InvalidPacket)),
        };

        if let Err(err) = ack.reason_code.as_result() {
            warn!("Broker rejected CONNECT with reason {}", ack.reason_code);
            return Err(Error::Peer(err));
        }

        let resumed = ack.session_present;
        if resumed && clean_start {
            return Err(Error::Peer(PeerError::InvalidPacket));
        }
        if !resumed {
            debug!("Broker started a fresh session; resetting local session state");
            self.data.reset();
        }

        let local_quota = self.data.outbound.max_inflight();
        let mut send_quota = local_quota;
        let mut max_send_quota = local_quota;
        let mut max_qos = None;
        let mut maximum_packet_size = None;
        let mut keepalive_interval = self.keepalive_interval;
        let mut assigned_client_id: Option<String<CLIENT_ID_CAPACITY>> = None;

        let property_result = (|| {
            for property in ack.properties.iter() {
                match property? {
                    Property::MaximumPacketSize(size) => maximum_packet_size = Some(size),
                    Property::AssignedClientIdentifier(id) => {
                        if !client_id.is_empty() || assigned_client_id.is_some() {
                            return Err(PeerError::InvalidPacket);
                        }
                        assigned_client_id =
                            Some(id.try_into().map_err(|_| PeerError::InvalidPacket)?);
                    }
                    Property::ServerKeepAlive(keepalive) => {
                        keepalive_interval = Duration::from_secs(keepalive as u64);
                    }
                    Property::ReceiveMaximum(max) => {
                        if max == 0 {
                            return Err(PeerError::InvalidPacket);
                        }
                        send_quota = max.min(local_quota);
                        max_send_quota = max.min(local_quota);
                    }
                    Property::MaximumQoS(max) => {
                        max_qos = Some(QoS::try_from(max).map_err(|_| PeerError::InvalidPacket)?);
                    }
                    _ => {}
                }
            }
            Ok(())
        })();
        property_result.map_err(Error::Peer)?;

        self.runtime.keepalive_interval = keepalive_interval;
        self.runtime.send_quota = send_quota;
        self.runtime.max_send_quota = max_send_quota;
        self.runtime.max_qos = max_qos;
        self.runtime.maximum_packet_size = maximum_packet_size;
        if let Some(assigned_client_id) = assigned_client_id {
            // The first CONNECT encoded an empty client ID, so the assigned ID adds only its bytes.
            let connect_workspace = connect_workspace + assigned_client_id.len();
            self.data
                .outbound
                .set_connect_workspace(connect_workspace)?;
            self.client_id = assigned_client_id;
        }

        debug!(
            "Activated session state resumed={=bool} send_quota={=u16}/{=u16} max_qos={=?} broker_max_packet_size={=?}",
            resumed,
            self.runtime.send_quota,
            self.runtime.max_send_quota,
            self.runtime.max_qos,
            self.runtime.maximum_packet_size
        );

        self.data.mark_session_present();
        self.runtime.note_outbound_activity(Instant::now());
        if resumed {
            info!("Connected and resumed existing broker session");
            Ok(ConnectEvent::Reconnected)
        } else {
            info!("Connected with a fresh broker session");
            Ok(ConnectEvent::Connected)
        }
    }
}
