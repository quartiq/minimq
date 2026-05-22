use embassy_time::Instant;
use embedded_io_async::Error as _;

use crate::mqtt_client::OpKind;
use crate::mqtt_client::outbound::{CONTROL_PACKET_LEN, write_all};
use crate::packets::{Disconnect, PublishHeader, Subscribe, Unsubscribe};
use crate::properties::{Properties, PropertyContext};
use crate::publication::{Publication, ToPayload};
use crate::ser::MqttSerializer;
use crate::types::TopicFilter;
use crate::wire::Utf8String;
use crate::{Error, Op, Property, PubError, QoS, ResourceError, debug, info, warn};

use super::{Io, Session};

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    /// Gracefully close the current MQTT transport with `DISCONNECT`.
    ///
    /// Cancel-safe if the underlying transport write/flush futures are cancel-safe.
    pub async fn disconnect(&mut self) -> Result<(), Error<IO::Error>> {
        self.disconnect_with(Disconnect::success()).await
    }

    /// Close the current MQTT transport with a caller-specified `DISCONNECT`.
    ///
    /// Use [`Disconnect::with_will`] to ask the broker to publish the configured Will immediately.
    ///
    /// Cancel-safe if the underlying transport write/flush futures are cancel-safe.
    pub async fn disconnect_with(
        &mut self,
        disconnect: Disconnect<'_>,
    ) -> Result<(), Error<IO::Error>> {
        if self.connection.is_none() {
            return Ok(());
        }
        info!("Graceful disconnect requested");
        if let Some(properties) = disconnect.properties()
            && !properties.valid_for(PropertyContext::Disconnect)
        {
            return Err(Error::InvalidRequest);
        }
        let mut buffer = [0u8; CONTROL_PACKET_LEN];
        let packet = MqttSerializer::encode(&mut buffer, &disconnect)?;
        self.runtime.require_packet_size(packet.len())?;
        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
        let result = match write_all(connection, packet).await {
            Ok(()) => connection.flush().await.map_err(Error::Transport),
            Err(err) => Err(err),
        };
        self.handle_disconnect();
        result
    }

    /// Send a `SUBSCRIBE`.
    ///
    /// Call this after [`connect`](Self::connect). A resumed [`crate::ConnectEvent::Reconnected`]
    /// already kept broker-side subscriptions.
    ///
    /// Returns an operation handle that can be checked with [`Session::is_pending`](Self::is_pending),
    /// [`Session::is_complete`](Self::is_complete), or
    /// [`Session::is_invalidated`](Self::is_invalidated).
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn subscribe(
        &mut self,
        topics: &[TopicFilter<'_>],
        properties: &[Property<'_>],
    ) -> Result<Op, Error<IO::Error>> {
        if self.connection.is_none() {
            return Err(Error::Disconnected);
        }
        if topics.is_empty() {
            return Err(Error::InvalidRequest);
        }
        if !Properties::from_slice(properties).valid_for(PropertyContext::Subscribe) {
            return Err(Error::InvalidRequest);
        }
        self.flush_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.data.next_packet_id();
        let (offset, len) = self.data.outbound.encode_packet(&Subscribe {
            packet_id,
            dup: false,
            properties: Properties::from_slice(properties),
            topics,
        })?;
        self.runtime.require_packet_size(len)?;
        self.data.outbound.retain_packet(packet_id, offset, len)?;
        debug!(
            "Enqueued SUBSCRIBE packet_id={=u16} len={=usize} tx_used={=usize}",
            packet_id,
            len,
            self.data.outbound.used()
        );
        self.flush_outbound().await?;
        Ok(Op::new(
            OpKind::Subscribe,
            packet_id,
            self.data.generation(),
        ))
    }

    /// Send an `UNSUBSCRIBE`.
    ///
    /// Returns an operation handle that can be checked with [`Session::is_pending`](Self::is_pending),
    /// [`Session::is_complete`](Self::is_complete), or
    /// [`Session::is_invalidated`](Self::is_invalidated).
    ///
    /// Cancel-safe if the underlying transport I/O futures are cancel-safe.
    pub async fn unsubscribe(
        &mut self,
        topics: &[&str],
        properties: &[Property<'_>],
    ) -> Result<Op, Error<IO::Error>> {
        if self.connection.is_none() {
            return Err(Error::Disconnected);
        }
        if topics.is_empty() {
            return Err(Error::InvalidRequest);
        }
        if !Properties::from_slice(properties).valid_for(PropertyContext::Unsubscribe) {
            return Err(Error::InvalidRequest);
        }
        self.flush_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.data.next_packet_id();
        let (offset, len) = self.data.outbound.encode_packet(&Unsubscribe {
            packet_id,
            dup: false,
            properties: Properties::from_slice(properties),
            topics,
        })?;
        self.runtime.require_packet_size(len)?;
        self.data.outbound.retain_packet(packet_id, offset, len)?;
        debug!(
            "Enqueued UNSUBSCRIBE packet_id={=u16} len={=usize} tx_used={=usize}",
            packet_id,
            len,
            self.data.outbound.used()
        );
        self.flush_outbound().await?;
        Ok(Op::new(
            OpKind::Unsubscribe,
            packet_id,
            self.data.generation(),
        ))
    }

    /// Send a `PUBLISH`.
    ///
    /// QoS 1 and 2 retain the encoded packet in the session TX buffer until broker ack and are
    /// cancel-safe if the underlying transport I/O futures are cancel-safe.
    /// They return `Some(Op)` so the caller can check completion with
    /// [`Session::is_pending`](Self::is_pending), [`Session::is_complete`](Self::is_complete),
    /// or [`Session::is_invalidated`](Self::is_invalidated).
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
        if self.connection.is_none() {
            return Err(Error::Disconnected.into());
        }
        self.flush_outbound().await?;

        let Publication {
            topic,
            properties,
            qos,
            payload,
            retain,
        } = publication;
        if !properties.valid_for(PropertyContext::Publish) {
            return Err(Error::InvalidRequest.into());
        }
        let qos = match self.runtime.max_qos {
            Some(max_qos) if self.downgrade_qos && qos > max_qos => max_qos,
            _ => qos,
        };
        let packet_id = (qos > QoS::AtMostOnce).then(|| self.data.next_packet_id());
        let header = PublishHeader {
            topic: Utf8String(topic),
            packet_id,
            properties,
            retain,
            qos,
            dup: false,
        };
        if packet_id.is_some() {
            self.require_retained_slot()?;
        }

        if !self.can_publish(qos) {
            return Err(Error::NotReady.into());
        }

        if let Some(packet_id) = packet_id {
            let (offset, len) = self.data.outbound.encode_publish(&header, payload)?;
            self.runtime.require_packet_size(len)?;
            self.data.outbound.retain_packet(packet_id, offset, len)?;
            self.runtime.send_quota = self.runtime.send_quota.saturating_sub(1);
            debug!(
                "Enqueued PUBLISH packet_id={=u16} qos={} len={=usize} send_quota={=u16}/{=u16} tx_used={=usize}",
                packet_id,
                qos,
                len,
                self.runtime.send_quota,
                self.runtime.max_send_quota,
                self.data.outbound.used()
            );
            self.flush_outbound().await?;
            let kind = if qos == QoS::ExactlyOnce {
                OpKind::PublishExactlyOnce
            } else {
                OpKind::PublishAtLeastOnce
            };
            return Ok(Some(Op::new(kind, packet_id, self.data.generation())));
        }

        let packet =
            MqttSerializer::encode_publish(self.data.outbound.scratch_space(), &header, payload)?;
        self.runtime.require_packet_size(packet.len())?;
        debug!("Sending QoS0 PUBLISH len={=usize}", packet.len());
        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
        if let Err(err) = write_all(connection, packet).await {
            if matches!(err, Error::WriteZero) {
                return Err(err.into());
            }
            warn!("QoS0 PUBLISH write failed");
            self.handle_disconnect();
            return Err(err.into());
        }
        if let Err(err) = connection.flush().await {
            warn!("QoS0 PUBLISH flush failed: {}", err.kind());
            self.handle_disconnect();
            return Err(Error::Transport(err).into());
        }
        self.runtime.note_outbound_activity(Instant::now());

        Ok(None)
    }

    pub(super) fn require_retained_slot(&self) -> Result<(), Error<IO::Error>> {
        if self.data.outbound.retained_full() {
            return Err(Error::Resource(ResourceError::InflightExhausted));
        }
        Ok(())
    }
}
