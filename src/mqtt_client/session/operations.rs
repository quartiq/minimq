use super::*;
use embedded_io_async::Error as _;

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    /// Gracefully close the current MQTT transport with `DISCONNECT`.
    ///
    /// Cancel-safe if the underlying transport write/flush futures are cancel-safe.
    pub async fn disconnect(&mut self) -> Result<(), Error<IO::Error>> {
        let Some(connection) = self.connection.as_mut() else {
            return Ok(());
        };
        info!("Graceful disconnect requested");
        let mut buffer = [0u8; 9];
        let result = write_packet(&mut buffer, connection, &DisconnectReq).await;
        self.handle_disconnect();
        result
    }

    /// Send a `SUBSCRIBE`.
    ///
    /// Call this after [`connect`](Self::connect). A resumed [`ConnectEvent::Reconnected`]
    /// already kept broker-side subscriptions.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<(), Error<IO::Error>> {
        if self.connection.is_none() {
            return Err(Error::Disconnected);
        }
        if topics.is_empty() {
            return Err(ProtocolError::NoTopic.into());
        }
        self.drive_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.data.next_packet_id();
        let (offset, len) = self
            .data
            .outbound
            .encode_packet(&Subscribe {
                packet_id,
                dup: false,
                properties: Properties::Slice(properties),
                topics,
            })
            .map_err(Error::Protocol)?;
        self.runtime.require_packet_size(len)?;
        self.data
            .outbound
            .retain_packet(packet_id, offset, len)
            .map_err(Error::Protocol)?;
        debug!(
            "Enqueued SUBSCRIBE packet_id={} len={} tx_used={}",
            packet_id,
            len,
            self.data.outbound.used()
        );
        self.drive_outbound().await
    }

    /// Send an `UNSUBSCRIBE`.
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<(), Error<IO::Error>> {
        if self.connection.is_none() {
            return Err(Error::Disconnected);
        }
        if topics.is_empty() {
            return Err(ProtocolError::NoTopic.into());
        }
        self.drive_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.data.next_packet_id();
        let (offset, len) = self
            .data
            .outbound
            .encode_packet(&Unsubscribe {
                packet_id,
                dup: false,
                properties: Properties::Slice(properties),
                topics,
            })
            .map_err(Error::Protocol)?;
        self.runtime.require_packet_size(len)?;
        self.data
            .outbound
            .retain_packet(packet_id, offset, len)
            .map_err(Error::Protocol)?;
        debug!(
            "Enqueued UNSUBSCRIBE packet_id={} len={} tx_used={}",
            packet_id,
            len,
            self.data.outbound.used()
        );
        self.drive_outbound().await
    }

    /// Send a `PUBLISH`.
    ///
    /// QoS 1 and 2 retain the encoded packet in the session TX buffer until broker ack and are
    /// cancel-safe if the underlying transport I/O futures are cancel-safe.
    ///
    /// QoS 0 bypasses retained outbound state, encodes into temporary TX scratch space, and writes
    /// directly to the transport. It therefore does not consume replay/in-flight slots and only
    /// needs enough currently free TX space for that one encode, but it is not cancel-safe.
    pub async fn publish<P>(
        &mut self,
        publication: crate::publication::Publication<'_, P>,
    ) -> Result<(), PubError<P::Error, IO::Error>>
    where
        P: crate::publication::ToPayload,
    {
        if self.connection.is_none() {
            return Err(PubError::Session(Error::Disconnected));
        }
        self.drive_outbound().await.map_err(PubError::Session)?;

        let mut publish: Pub<'_, P> = publication.into();
        if let Some(max_qos) = self.runtime.max_qos
            && self.downgrade_qos
            && publish.qos > max_qos
        {
            publish.qos = max_qos;
        }

        publish.packet_id = (publish.qos > QoS::AtMostOnce).then(|| self.data.next_packet_id());
        publish.dup = false;

        let packet_id = publish.packet_id;
        let qos = publish.qos;
        if packet_id.is_some() {
            self.require_retained_slot().map_err(PubError::Session)?;
        }

        if !self.can_publish(qos) {
            return Err(PubError::Session(Error::NotReady));
        }

        if let Some(packet_id) = packet_id {
            let (offset, len) = self.data.outbound.encode_publish(publish)?;
            self.runtime
                .require_packet_size(len)
                .map_err(PubError::Session)?;
            self.data
                .outbound
                .retain_packet(packet_id, offset, len)
                .map_err(|err| PubError::Session(Error::Protocol(err)))?;
            self.runtime.send_quota = self.runtime.send_quota.saturating_sub(1);
            debug!(
                "Enqueued PUBLISH packet_id={} qos={:?} len={} send_quota={}/{} tx_used={}",
                packet_id,
                qos,
                len,
                self.runtime.send_quota,
                self.runtime.max_send_quota,
                self.data.outbound.used()
            );
            self.drive_outbound().await.map_err(PubError::Session)?;
            return Ok(());
        }

        let packet =
            crate::ser::MqttSerializer::pub_to_buffer(self.data.outbound.scratch_space(), publish)
                .map_err(|err| match err {
                    crate::ser::PubError::Encode(err) => {
                        PubError::Session(Error::Protocol(err.into()))
                    }
                    crate::ser::PubError::Payload(err) => PubError::Payload(err),
                })?;
        self.runtime
            .require_packet_size(packet.len())
            .map_err(PubError::Session)?;
        debug!("Sending QoS0 PUBLISH len={}", packet.len());
        if let Err(err) = {
            let connection = self
                .connection
                .as_mut()
                .ok_or(PubError::Session(Error::Disconnected))?;
            crate::mqtt_client::outbound::write_all(connection, packet).await
        } {
            warn!("QoS0 PUBLISH write failed");
            self.handle_disconnect();
            return Err(PubError::Session(err));
        }
        if let Err(err) = {
            let connection = self
                .connection
                .as_mut()
                .ok_or(PubError::Session(Error::Disconnected))?;
            connection.flush().await
        } {
            warn!("QoS0 PUBLISH flush failed: {:?}", err.kind());
            self.handle_disconnect();
            return Err(PubError::Session(Error::Transport(err)));
        }
        self.runtime.note_outbound_activity(Instant::now());

        Ok(())
    }

    pub(super) fn require_retained_slot(&self) -> Result<(), Error<IO::Error>> {
        if self.data.outbound.retained_full() {
            return Err(ProtocolError::InflightMetadataExhausted.into());
        }
        Ok(())
    }
}
