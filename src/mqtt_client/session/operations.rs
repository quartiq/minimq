use embassy_time::Instant;
use embedded_io_async::Error as _;

use crate::packets::{DisconnectReq, PublishHeader, Subscribe, Unsubscribe};
use crate::types::{Properties, TopicFilter};
use crate::{Error, Op, Property, PubError, QoS, ResourceError, debug, info, warn};

use super::super::outbound::write_packet;
use super::{Io, Session};

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
    /// Returns an operation handle that can be checked with [`Session::status`](Self::status).
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
        self.flush_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.data.next_packet_id();
        let (offset, len) = self.data.outbound.encode_packet(&Subscribe {
            packet_id,
            dup: false,
            properties: Properties::Slice(properties),
            topics,
        })?;
        self.runtime.require_packet_size(len)?;
        self.data.outbound.retain_packet(packet_id, offset, len)?;
        debug!(
            "Enqueued SUBSCRIBE packet_id={} len={} tx_used={}",
            packet_id,
            len,
            self.data.outbound.used()
        );
        self.flush_outbound().await?;
        Ok(Op::new(
            super::super::OpKind::Subscribe,
            packet_id,
            self.data.generation(),
        ))
    }

    /// Send an `UNSUBSCRIBE`.
    ///
    /// Returns an operation handle that can be checked with [`Session::status`](Self::status).
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
        self.flush_outbound().await?;
        self.require_retained_slot()?;

        let packet_id = self.data.next_packet_id();
        let (offset, len) = self.data.outbound.encode_packet(&Unsubscribe {
            packet_id,
            dup: false,
            properties: Properties::Slice(properties),
            topics,
        })?;
        self.runtime.require_packet_size(len)?;
        self.data.outbound.retain_packet(packet_id, offset, len)?;
        debug!(
            "Enqueued UNSUBSCRIBE packet_id={} len={} tx_used={}",
            packet_id,
            len,
            self.data.outbound.used()
        );
        self.flush_outbound().await?;
        Ok(Op::new(
            super::super::OpKind::Unsubscribe,
            packet_id,
            self.data.generation(),
        ))
    }

    /// Send a `PUBLISH`.
    ///
    /// QoS 1 and 2 retain the encoded packet in the session TX buffer until broker ack and are
    /// cancel-safe if the underlying transport I/O futures are cancel-safe.
    /// They return `Some(Op)` so the caller can check completion with
    /// [`Session::status`](Self::status).
    ///
    /// QoS 0 bypasses retained outbound state, encodes into temporary TX scratch space, and writes
    /// directly to the transport. It therefore does not consume replay/in-flight slots and only
    /// needs enough currently free TX space for that one encode, but it is not cancel-safe and
    /// returns `None`.
    pub async fn publish<P>(
        &mut self,
        publication: crate::publication::Publication<'_, P>,
    ) -> Result<Option<Op>, PubError<P::Error, IO::Error>>
    where
        P: crate::publication::ToPayload,
    {
        if self.connection.is_none() {
            return Err(Error::Disconnected.into());
        }
        self.flush_outbound().await?;

        let crate::publication::Publication {
            topic,
            properties,
            qos,
            payload,
            retain,
        } = publication;
        let qos = match self.runtime.max_qos {
            Some(max_qos) if self.downgrade_qos && qos > max_qos => max_qos,
            _ => qos,
        };
        let packet_id = (qos > QoS::AtMostOnce).then(|| self.data.next_packet_id());
        let header = PublishHeader {
            topic: crate::types::Utf8String(topic),
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
                "Enqueued PUBLISH packet_id={} qos={:?} len={} send_quota={}/{} tx_used={}",
                packet_id,
                qos,
                len,
                self.runtime.send_quota,
                self.runtime.max_send_quota,
                self.data.outbound.used()
            );
            self.flush_outbound().await?;
            let kind = if qos == QoS::ExactlyOnce {
                super::super::OpKind::PublishExactlyOnce
            } else {
                super::super::OpKind::PublishAtLeastOnce
            };
            return Ok(Some(Op::new(kind, packet_id, self.data.generation())));
        }

        let packet = crate::ser::MqttSerializer::pub_to_buffer(
            self.data.outbound.scratch_space(),
            &header,
            payload,
        )?;
        self.runtime.require_packet_size(packet.len())?;
        debug!("Sending QoS0 PUBLISH len={}", packet.len());
        let connection = self.connection.as_mut().ok_or(Error::Disconnected)?;
        if let Err(err) = crate::mqtt_client::outbound::write_all(connection, packet).await {
            if matches!(err, Error::WriteZero) {
                return Err(err.into());
            }
            warn!("QoS0 PUBLISH write failed");
            self.handle_disconnect();
            return Err(err.into());
        }
        if let Err(err) = connection.flush().await {
            warn!("QoS0 PUBLISH flush failed: {:?}", err.kind());
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
