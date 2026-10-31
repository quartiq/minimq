use core::convert::Infallible;

use crate::de::ReceivedPacket;
use crate::mqtt_client::outbound::{ControlAction, PacketKind, check_control_packet_size};
use crate::{
    Connection, Error, Event, InboundPublish, Io, Op, Outcome, PeerError, ProtocolError, QoS,
    ReasonCode, ResourceError, SubscribeResults, UnsubscribeResults, debug, info, trace, warn,
};

use super::state::{RuntimeState, SessionData};

impl<'a> SessionData<'a> {
    pub(super) fn handle_packet(
        &mut self,
        runtime: &mut RuntimeState,
        packet: ReceivedPacket<'_>,
        protocol: crate::Protocol,
    ) -> Result<bool, Error<Infallible>> {
        match packet {
            ReceivedPacket::ConnAck(_) => return Err(ProtocolError::UnexpectedPacket.into()),
            ReceivedPacket::SubAck(ack) => {
                if !self.outbound.ack_subscription(
                    PacketKind::Subscribe,
                    ack.packet_id,
                    ack.codes,
                    protocol,
                )? {
                    debug!("Ignoring stale SUBACK for packet id {=u16}", ack.packet_id);
                    return Ok(false);
                }
                debug!("Processed SUBACK packet_id={=u16}", ack.packet_id);
                return Ok(true);
            }
            ReceivedPacket::UnsubAck(ack) => {
                if !self.outbound.ack_subscription(
                    PacketKind::Unsubscribe,
                    ack.packet_id,
                    ack.codes,
                    protocol,
                )? {
                    debug!(
                        "Ignoring stale UNSUBACK for packet id {=u16}",
                        ack.packet_id
                    );
                    return Ok(false);
                }
                debug!("Processed UNSUBACK packet_id={=u16}", ack.packet_id);
                return Ok(true);
            }
            ReceivedPacket::PingResp => {
                trace!("Received PINGRESP");
                runtime.ping_timeout = None;
            }
            ReceivedPacket::PubAck(ack) => {
                if !self
                    .outbound
                    .ack_packet(PacketKind::PublishAtLeastOnce, ack.packet_id)
                {
                    debug!("Ignoring stale PUBACK for packet id {=u16}", ack.packet_id);
                    return Ok(false);
                }
                runtime.send_quota = runtime
                    .send_quota
                    .saturating_add(1)
                    .min(runtime.max_send_quota);
                debug!(
                    "Processed PUBACK packet_id={=u16} send_quota={=u16}",
                    ack.packet_id, runtime.send_quota
                );
                return Ok(true);
            }
            ReceivedPacket::PubRec(rec) => {
                if !rec.reason.code().success() {
                    if self
                        .outbound
                        .ack_packet(PacketKind::PublishExactlyOnce, rec.packet_id)
                    {
                        runtime.send_quota = runtime
                            .send_quota
                            .saturating_add(1)
                            .min(runtime.max_send_quota);
                        return Ok(true);
                    }
                } else {
                    check_control_packet_size(
                        runtime.maximum_packet_size,
                        ControlAction::PubRel {
                            packet_id: rec.packet_id,
                        },
                        protocol,
                    )?;
                    self.outbound.release(rec.packet_id);
                    debug!("Queued PUBREL for packet_id={=u16}", rec.packet_id);
                }
            }
            ReceivedPacket::PubComp(comp) => {
                if !self.outbound.ack_packet(PacketKind::PubRel, comp.packet_id) {
                    debug!(
                        "Ignoring stale PUBCOMP for packet id {=u16}",
                        comp.packet_id
                    );
                    return Ok(false);
                }
                debug!("Processed PUBCOMP packet_id={=u16}", comp.packet_id);
                runtime.send_quota = runtime
                    .send_quota
                    .saturating_add(1)
                    .min(runtime.max_send_quota);
                return Ok(true);
            }
            ReceivedPacket::PubRel(rel) => {
                let position = self
                    .pending_server_packet_ids
                    .iter()
                    .position(|id| *id == rel.packet_id);
                let reason = if position.is_some() {
                    ReasonCode::Success
                } else {
                    ReasonCode::PacketIdNotFound
                };
                debug!(
                    "Queueing PUBCOMP for inbound PUBREL packet_id={=u16} reason={} pending_inbound_qos2={=usize}",
                    rel.packet_id,
                    reason,
                    self.pending_server_packet_ids.len()
                );
                let action = ControlAction::PubComp {
                    packet_id: rel.packet_id,
                    reason,
                };
                check_control_packet_size(runtime.maximum_packet_size, action, protocol)?;
                self.outbound.queue_control(action)?;
                if let Some(index) = position {
                    self.pending_server_packet_ids.swap_remove(index);
                }
            }
            ReceivedPacket::Publish(info) => {
                if protocol.is_v5()
                    && let Some(id) = info.packet_id
                    && !runtime.receiving.contains(&id)
                {
                    runtime
                        .receiving
                        .push(id)
                        .map_err(|_| PeerError::InvalidPacket)?;
                }
                debug!(
                    "Handling inbound PUBLISH packet_id={=?} topic={=str} qos={} retain={} payload_len={=usize}",
                    info.packet_id,
                    info.topic.0,
                    info.qos,
                    info.retain,
                    info.payload.len()
                );
                match info.qos {
                    QoS::AtMostOnce => {}
                    QoS::AtLeastOnce => {
                        let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                        let reason = if self.pending_server_packet_ids.contains(&packet_id) {
                            ReasonCode::PacketIdInUse
                        } else {
                            ReasonCode::Success
                        };
                        trace!(
                            "Queueing PUBACK for inbound QoS1 PUBLISH packet_id={=u16} {}",
                            packet_id, reason
                        );
                        let action = ControlAction::PubAck { packet_id, reason };
                        if !protocol.is_v5() && !reason.success() {
                            return Err(PeerError::InvalidPacket.into());
                        }
                        check_control_packet_size(runtime.maximum_packet_size, action, protocol)?;
                        self.outbound.queue_control(action)?;
                        if !reason.success() {
                            return Ok(false);
                        }
                    }
                    QoS::ExactlyOnce => {
                        let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                        let duplicate = self.pending_server_packet_ids.contains(&packet_id);
                        if !duplicate && self.pending_server_packet_ids.is_full() {
                            if protocol.is_v5() {
                                let action = ControlAction::PubRec {
                                    packet_id,
                                    reason: ReasonCode::QuotaExceeded,
                                };
                                check_control_packet_size(
                                    runtime.maximum_packet_size,
                                    action,
                                    protocol,
                                )?;
                                self.outbound.queue_control(action)?;
                            }
                            return Err(ResourceError::InflightExhausted.into());
                        }
                        trace!(
                            "Queueing PUBREC for inbound QoS2 PUBLISH packet_id={=u16} duplicate={=bool}",
                            packet_id, duplicate
                        );
                        let action = ControlAction::PubRec {
                            packet_id,
                            reason: crate::ReasonCode::Success,
                        };
                        check_control_packet_size(runtime.maximum_packet_size, action, protocol)?;
                        self.outbound.queue_control(action)?;
                        if duplicate {
                            return Ok(false);
                        }
                        // Capacity was checked above. Commit duplicate suppression only
                        // after acknowledgement admission succeeds, just before delivery.
                        self.pending_server_packet_ids
                            .push(packet_id)
                            .map_err(|_| PeerError::InvalidPacket)?;
                    }
                }
                return Ok(true);
            }
            ReceivedPacket::Disconnect(disconnect) => {
                info!(
                    "Received broker DISCONNECT reason={}",
                    disconnect.reason_code()
                );
                return Err(Error::Disconnected);
            }
        }

        Ok(false)
    }
}

impl<'buf, IO: Io> Connection<'_, 'buf, IO> {
    pub(super) fn process_received_packet(&mut self) -> Result<Option<usize>, Error<IO::Error>> {
        if !self.session.packet_reader.packet_available() {
            return Ok(None);
        }

        let (packet_length, packet) = match self
            .session
            .packet_reader
            .take_packet(self.session.protocol)
        {
            Ok(packet) => packet,
            Err(err) => {
                warn!("Failed to decode inbound packet: {}", err);
                self.handle_disconnect();
                return Err(err.into());
            }
        };
        match self.session.data.handle_packet(
            &mut self.session.runtime,
            packet,
            self.session.protocol,
        ) {
            Ok(true) => Ok(Some(packet_length)),
            Ok(false) => Ok(None),
            Err(Error::Disconnected) => {
                warn!("Disconnecting session after broker DISCONNECT");
                self.handle_disconnect();
                Err(Error::Disconnected)
            }
            Err(Error::Peer(PeerError::InvalidPacket)) => {
                warn!("Disconnecting session after packet handling error");
                self.handle_disconnect();
                Err(Error::Peer(PeerError::InvalidPacket))
            }
            Err(Error::Resource(ResourceError::PacketTooLarge)) => {
                warn!("Disconnecting session after packet handling error");
                self.handle_disconnect();
                Err(Error::Resource(ResourceError::PacketTooLarge))
            }
            Err(Error::Peer(err)) => Err(Error::Peer(err)),
            Err(Error::Resource(err)) => {
                if !self.session.protocol.is_v5() {
                    self.handle_disconnect();
                }
                Err(Error::Resource(err))
            }
            Err(Error::InvalidRequest) => Err(Error::InvalidRequest),
            Err(Error::NotReady) => Err(Error::NotReady),
            Err(Error::WriteZero) => Err(Error::WriteZero),
            Err(Error::Transport(never)) => match never {},
        }
    }

    pub(super) fn decode_event(&self, packet_length: usize) -> Result<Event<'_>, ProtocolError> {
        let buffer = self
            .session
            .packet_reader
            .buffer
            .get(..packet_length)
            .ok_or(ProtocolError::MalformedPacket)?;
        let packet = ReceivedPacket::from_buffer(buffer, self.session.protocol)?;
        let (packet_id, outcome) = match packet {
            ReceivedPacket::Publish(info) => {
                return Ok(Event::Publish(InboundPublish::new(
                    info.topic.0,
                    info.payload,
                    info.properties,
                    info.retain,
                    info.qos,
                )));
            }
            ReceivedPacket::SubAck(ack) => (
                ack.packet_id,
                Outcome::Subscribe(SubscribeResults(ack.codes)),
            ),
            ReceivedPacket::UnsubAck(ack) => (
                ack.packet_id,
                Outcome::Unsubscribe(UnsubscribeResults(
                    self.session.protocol.is_v5().then_some(ack.codes),
                )),
            ),
            ReceivedPacket::PubAck(ack) => (
                ack.packet_id,
                Outcome::Publish(super::super::broker_result(ack.reason.code())),
            ),
            ReceivedPacket::PubRec(ack) => (
                ack.packet_id,
                Outcome::Publish(super::super::broker_result(ack.reason.code())),
            ),
            ReceivedPacket::PubComp(ack) => (
                ack.packet_id,
                Outcome::Publish(super::super::broker_result(ack.reason.code())),
            ),
            _ => return Err(ProtocolError::UnexpectedPacket),
        };
        Ok(Event::Completed {
            op: Op::new(packet_id, self.session.data.generation()),
            outcome,
        })
    }
}
