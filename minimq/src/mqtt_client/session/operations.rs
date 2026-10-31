use embassy_time::Instant;
use embedded_io_async::Error as _;

use crate::mqtt_client::outbound::{PacketKind, write_all};
use crate::packets::{Disconnect, PublishHeader, Subscribe, Unsubscribe};
use crate::properties::{Properties, PropertyContext};
use crate::publication::{Publication, ToPayload};
use crate::ser::MqttSerializer;
use crate::types::TopicFilter;
use crate::wire::Utf8String;
use crate::{Connection, Error, Io, Op, Property, PubError, QoS, ResourceError, debug, info, warn};

impl<'buf, IO: Io> Connection<'_, 'buf, IO> {
    /// Write the `DISCONNECT` over the transport. The caller drops the transport afterwards.
    ///
    /// Drop the connection if this operation is cancelled.
    pub async fn disconnect_with(
        &mut self,
        disconnect: Disconnect<'_>,
    ) -> Result<(), Error<IO::Error>> {
        if !self.live {
            return Ok(());
        }
        info!("Graceful disconnect requested");
        let disconnect = if self.session.protocol.is_v5() {
            disconnect
        } else {
            if disconnect.reason_code() != crate::ReasonCode::Success
                || disconnect
                    .properties()
                    .is_some_and(|properties| properties.size() != 0)
            {
                return Err(Error::InvalidRequest);
            }
            Disconnect::success()
        };
        if let Some(properties) = disconnect.properties()
            && (!properties.valid_for(PropertyContext::Disconnect)
                || (self.session.session_expiry_interval.unwrap_or(0) == 0
                    && properties
                        .iter()
                        .any(|p| matches!(p, Ok(Property::SessionExpiryInterval(1..))))))
        {
            return Err(Error::InvalidRequest);
        }
        // A cancelled publication can leave a packet prefix or suspended flush.
        // Finish that stream progress before writing the terminal packet.
        self.flush_outbound().await?;
        let packet =
            MqttSerializer::encode(self.session.data.outbound.scratch_space(), &disconnect)?;
        self.session.runtime.require_packet_size(packet.len())?;
        let result = match write_all(&mut self.io, packet).await {
            Ok(()) => self.io.flush().await.map_err(Error::Transport),
            Err(err) => Err(err),
        };
        // The transport is finished after a DISCONNECT regardless of the write outcome.
        self.handle_disconnect();
        result
    }

    /// Gracefully end the MQTT connection with `DISCONNECT`.
    ///
    /// This is the graceful counterpart to simply dropping the handle: it sends the MQTT
    /// `DISCONNECT` so the broker closes cleanly and suppresses the Will. Just dropping
    /// the handle skips this and is treated by the broker as an abnormal disconnect.
    /// Drop the connection if this operation is cancelled.
    pub async fn disconnect(&mut self) -> Result<(), Error<IO::Error>> {
        self.disconnect_with(Disconnect::success()).await
    }

    /// Send a `SUBSCRIBE`.
    ///
    /// Call this after [`Session::connect`](crate::Session::connect). A resumed
    /// [`ConnectEvent::SessionResumed`](crate::ConnectEvent::SessionResumed)
    /// already kept broker-side subscriptions.
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<Op, Error<IO::Error>> {
        if !self.live {
            return Err(Error::Disconnected);
        }
        if topics.is_empty() {
            return Err(Error::InvalidRequest);
        }
        if !self.session.protocol.is_v5()
            && (!properties.is_empty() || topics.iter().any(|topic| !topic.valid_for_v3()))
        {
            return Err(Error::InvalidRequest);
        }
        if !Properties::from_slice(properties).valid_for(PropertyContext::Subscribe) {
            return Err(Error::InvalidRequest);
        }
        let capabilities = self.session.runtime.capabilities;
        if topics.iter().any(|topic| !topic.allowed_by(capabilities))
            || (!capabilities.subscription_ids
                && properties
                    .iter()
                    .any(|p| matches!(p, Property::SubscriptionIdentifier(_))))
        {
            return Err(Error::InvalidRequest);
        }
        self.flush_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.session.data.next_packet_id();
        let len = self.session.data.outbound.encode_packet(&Subscribe {
            packet_id,
            properties: self
                .session
                .protocol
                .is_v5()
                .then(|| Properties::from_slice(properties)),
            topics,
        })?;
        self.session.runtime.require_packet_size(len)?;
        self.session.data.outbound.retain_packet(
            PacketKind::Subscribe,
            packet_id,
            len,
            topics.len(),
        )?;
        debug!(
            "Enqueued SUBSCRIBE packet_id={=u16} len={=usize} tx_used={=usize}",
            packet_id,
            len,
            self.session.data.outbound.retained_bytes()
        );
        self.flush_outbound().await?;
        Ok(Op::new(packet_id, self.session.data.generation()))
    }

    /// Send an `UNSUBSCRIBE`.
    ///
    /// Returns the handle correlated with the eventual [`crate::Event::Completed`].
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<Op, Error<IO::Error>> {
        if !self.live {
            return Err(Error::Disconnected);
        }
        if topics.is_empty() {
            return Err(Error::InvalidRequest);
        }
        if !Properties::from_slice(properties).valid_for(PropertyContext::Unsubscribe) {
            return Err(Error::InvalidRequest);
        }
        if !self.session.protocol.is_v5() && !properties.is_empty() {
            return Err(Error::InvalidRequest);
        }
        self.flush_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.session.data.next_packet_id();
        let len = self.session.data.outbound.encode_packet(&Unsubscribe {
            packet_id,
            properties: self
                .session
                .protocol
                .is_v5()
                .then(|| Properties::from_slice(properties)),
            topics,
        })?;
        self.session.runtime.require_packet_size(len)?;
        self.session.data.outbound.retain_packet(
            PacketKind::Unsubscribe,
            packet_id,
            len,
            topics.len(),
        )?;
        debug!(
            "Enqueued UNSUBSCRIBE packet_id={=u16} len={=usize} tx_used={=usize}",
            packet_id,
            len,
            self.session.data.outbound.retained_bytes()
        );
        self.flush_outbound().await?;
        Ok(Op::new(packet_id, self.session.data.generation()))
    }

    /// Send a `PUBLISH`.
    ///
    /// QoS 1/2 publications join any queued outbound acknowledgements before one transport flush.
    /// A suspended flush is resumed before further writes. Admission happens before transport I/O,
    /// so cancellation can retain a publication without returning its operation handle.
    ///
    /// QoS 1 and 2 retain the encoded packet in the session TX buffer until broker ack and are
    /// cancel-safe if the underlying transport I/O futures are cancel-safe.
    /// They return `Some(Op)`, correlated with the eventual [`crate::Event::Completed`].
    ///
    /// QoS 0 bypasses retained outbound state, encodes into temporary TX scratch space, and writes
    /// directly to the transport. It therefore does not consume replay/in-flight slots and only
    /// needs enough currently free TX space for that one encode, but it is not cancel-safe and
    /// returns `None`.
    pub async fn publish<P>(
        &mut self,
        publication: Publication<'_, P>,
    ) -> Result<Option<Op>, PubError<P::Error, IO::Error>>
    where
        P: ToPayload,
    {
        if !self.live {
            return Err(Error::Disconnected.into());
        }

        let Publication {
            topic,
            properties,
            qos,
            payload,
            retain,
        } = publication;
        if !properties.valid_for(PropertyContext::Publish)
            || !crate::wire::valid_topic(topic)
            || (retain == crate::Retain::Retained && !self.session.runtime.capabilities.retain)
        {
            return Err(Error::InvalidRequest.into());
        }
        if !self.session.protocol.is_v5() && properties.size() != 0 {
            return Err(Error::InvalidRequest.into());
        }
        let qos = match self.session.runtime.max_qos {
            Some(max_qos) if qos > max_qos && self.session.downgrade_qos => max_qos,
            Some(max_qos) if qos > max_qos => return Err(Error::InvalidRequest.into()),
            _ => qos,
        };
        // One failure path keeps flush/resume state shared across admission errors.
        let admission = (|| {
            if qos > QoS::AtMostOnce {
                self.require_retained_slot()?;
            }
            if !self.can_publish(qos) {
                return Err(Error::NotReady);
            }
            Ok(())
        })();
        if let Err(err) = admission {
            self.flush_outbound().await?;
            return Err(err.into());
        }

        let packet_id = (qos > QoS::AtMostOnce).then(|| self.session.data.next_packet_id());
        let header = PublishHeader {
            topic: Utf8String(topic),
            packet_id,
            properties: self.session.protocol.is_v5().then_some(properties),
            retain,
            qos,
            dup: false,
        };
        if let Some(packet_id) = packet_id {
            let encoded = self
                .session
                .data
                .outbound
                .encode_publish(&header, payload)
                .and_then(|len| {
                    self.session.runtime.require_packet_size(len)?;
                    Ok(len)
                });
            let len = match encoded {
                Ok(len) => len,
                Err(err) => {
                    self.flush_outbound().await?;
                    return Err(err);
                }
            };
            return self
                .send_publish(packet_id, len, qos)
                .await
                .map(Some)
                .map_err(Into::into);
        }

        self.flush_outbound().await?;
        let (offset, packet) = MqttSerializer::encode_publish_with_offset(
            self.session.data.outbound.scratch_space(),
            &header,
            payload,
        )?;
        let len = packet.len();
        self.send_qos0(offset, len).await?;
        Ok(None)
    }

    async fn send_publish(
        &mut self,
        packet_id: u16,
        len: usize,
        qos: QoS,
    ) -> Result<Op, Error<IO::Error>> {
        let kind = if qos == QoS::ExactlyOnce {
            PacketKind::PublishExactlyOnce
        } else {
            PacketKind::PublishAtLeastOnce
        };
        self.session
            .data
            .outbound
            .retain_packet(kind, packet_id, len, 0)?;
        debug!(
            "Enqueued PUBLISH packet_id={=u16} qos={} len={=usize} send_quota={=u16}/{=u16} tx_used={=usize}",
            packet_id,
            qos,
            len,
            self.session.runtime.send_quota,
            self.session.runtime.max_send_quota,
            self.session.data.outbound.retained_bytes()
        );
        self.flush_outbound().await?;
        Ok(Op::new(packet_id, self.session.data.generation()))
    }

    async fn send_qos0(&mut self, offset: usize, len: usize) -> Result<(), Error<IO::Error>> {
        let packet = &self.session.data.outbound.scratch_space()[offset..offset + len];
        self.session.runtime.require_packet_size(len)?;
        debug!("Sending QoS0 PUBLISH len={=usize}", len);
        if !self.live {
            return Err(Error::Disconnected);
        }
        if let Err(err) = write_all(&mut self.io, packet).await {
            warn!("QoS0 PUBLISH write failed");
            self.handle_disconnect();
            return Err(err);
        }
        if let Err(err) = self.io.flush().await {
            warn!("QoS0 PUBLISH flush failed: {}", err.kind());
            self.handle_disconnect();
            return Err(Error::Transport(err));
        }
        self.session.runtime.note_outbound_activity(Instant::now());

        Ok(())
    }

    pub(super) fn require_retained_slot(&self) -> Result<(), Error<IO::Error>> {
        if self.session.data.outbound.retained_full() {
            return Err(Error::Resource(ResourceError::InflightExhausted));
        }
        Ok(())
    }
}
