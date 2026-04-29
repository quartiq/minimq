use core::convert::Infallible;

use crate::de::received_packet::ReceivedPacket;
use crate::{Error, InboundPublish, ProtocolError, QoS, ReasonCode, debug, info, trace, warn};

use super::super::outbound::{ControlAction, check_control_packet_size, check_pubrel_size};
use super::state::{RuntimeState, SessionData};
use super::{Io, Session};

impl<'a> SessionData<'a> {
    pub(super) fn handle_packet(
        &mut self,
        runtime: &mut RuntimeState,
        packet: ReceivedPacket<'_>,
    ) -> Result<bool, Error<Infallible>> {
        match packet {
            ReceivedPacket::ConnAck(_) => return Err(ProtocolError::UnexpectedPacket.into()),
            ReceivedPacket::SubAck(ack) => {
                if !self.outbound.ack_packet(ack.packet_identifier) {
                    debug!(
                        "Ignoring stale SUBACK for packet id {}",
                        ack.packet_identifier
                    );
                    return Ok(false);
                }
                debug!("Processed SUBACK packet_id={}", ack.packet_identifier);
                for &code in ack.codes {
                    ReasonCode::from(code).as_result()?;
                }
            }
            ReceivedPacket::UnsubAck(ack) => {
                if !self.outbound.ack_packet(ack.packet_identifier) {
                    debug!(
                        "Ignoring stale UNSUBACK for packet id {}",
                        ack.packet_identifier
                    );
                    return Ok(false);
                }
                debug!("Processed UNSUBACK packet_id={}", ack.packet_identifier);
                for &code in ack.codes {
                    ReasonCode::from(code).as_result()?;
                }
            }
            ReceivedPacket::PingResp => {
                trace!("Received PINGRESP");
                runtime.ping_timeout = None;
            }
            ReceivedPacket::PubAck(ack) => {
                if !self.outbound.ack_packet(ack.packet_identifier) {
                    debug!(
                        "Ignoring stale PUBACK for packet id {}",
                        ack.packet_identifier
                    );
                    return Ok(false);
                }
                runtime.send_quota = runtime
                    .send_quota
                    .saturating_add(1)
                    .min(runtime.max_send_quota);
                debug!(
                    "Processed PUBACK packet_id={} send_quota={}",
                    ack.packet_identifier, runtime.send_quota
                );
                ack.reason.code().as_result()?;
            }
            ReceivedPacket::PubRec(rec) => {
                let queue_release = match self.outbound.ack_packet(rec.packet_id) {
                    true => {
                        runtime.send_quota = runtime
                            .send_quota
                            .saturating_add(1)
                            .min(runtime.max_send_quota);
                        debug!(
                            "Processed PUBREC packet_id={} send_quota={}",
                            rec.packet_id, runtime.send_quota
                        );
                        true
                    }
                    false if self.outbound.has_pending_release(rec.packet_id) => {
                        debug!(
                            "Replaying PUBREL after stale PUBREC for packet id {}",
                            rec.packet_id
                        );
                        false
                    }
                    false => {
                        debug!("Ignoring stale PUBREC for packet id {}", rec.packet_id);
                        return Ok(false);
                    }
                };
                rec.reason.code().as_result()?;
                if queue_release {
                    check_pubrel_size(
                        runtime.maximum_packet_size,
                        rec.packet_id,
                        ReasonCode::Success,
                    )?;
                    self.outbound
                        .queue_release(rec.packet_id, ReasonCode::Success)?;
                    debug!("Queued PUBREL for packet_id={}", rec.packet_id);
                }
            }
            ReceivedPacket::PubComp(comp) => {
                if !self.outbound.ack_release(comp.packet_id) {
                    debug!("Ignoring stale PUBCOMP for packet id {}", comp.packet_id);
                    return Ok(false);
                }
                debug!("Processed PUBCOMP packet_id={}", comp.packet_id);
                comp.reason.code().as_result()?;
            }
            ReceivedPacket::PubRel(rel) => {
                let reason = if let Some(index) = self
                    .pending_server_packet_ids
                    .iter()
                    .position(|id| *id == rel.packet_id)
                {
                    self.pending_server_packet_ids.swap_remove(index);
                    ReasonCode::Success
                } else {
                    ReasonCode::PacketIdNotFound
                };
                debug!(
                    "Queueing PUBCOMP for inbound PUBREL packet_id={} reason={:?} pending_inbound_qos2={}",
                    rel.packet_id,
                    reason,
                    self.pending_server_packet_ids.len()
                );
                let action = ControlAction::PubComp {
                    packet_id: rel.packet_id,
                    reason,
                };
                check_control_packet_size(runtime.maximum_packet_size, action)?;
                self.outbound.queue_control(action)?;
            }
            ReceivedPacket::Publish(info) => {
                debug!(
                    "Handling inbound PUBLISH packet_id={:?} topic={} qos={:?} retain={:?} payload_len={}",
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
                            "Queueing PUBACK for inbound QoS1 PUBLISH packet_id={} {:?}",
                            packet_id, reason
                        );
                        let action = ControlAction::PubAck { packet_id, reason };
                        check_control_packet_size(runtime.maximum_packet_size, action)?;
                        self.outbound.queue_control(action)?;
                    }
                    QoS::ExactlyOnce => {
                        let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                        let duplicate = self.pending_server_packet_ids.contains(&packet_id);
                        let reason = if !duplicate {
                            self.pending_server_packet_ids
                                .push(packet_id)
                                .map(|_| ReasonCode::Success)
                                .unwrap_or(ReasonCode::ReceiveMaxExceeded)
                        } else {
                            ReasonCode::Success
                        };
                        trace!(
                            "Queueing PUBREC for inbound QoS2 PUBLISH packet_id={} duplicate={} {:?}",
                            packet_id, duplicate, reason
                        );
                        let action = ControlAction::PubRec { packet_id, reason };
                        check_control_packet_size(runtime.maximum_packet_size, action)?;
                        self.outbound.queue_control(action)?;
                        if duplicate || !reason.success() {
                            debug!(
                                "Ignoring inbound QoS2 PUBLISH after PUBREC packet_id={} duplicate={} reason={:?}",
                                packet_id, duplicate, reason
                            );
                            return Ok(false);
                        }
                    }
                }
                return Ok(true);
            }
            ReceivedPacket::Disconnect(_) => {
                info!("Received broker DISCONNECT");
                return Err(Error::Disconnected);
            }
        }

        Ok(false)
    }
}

impl<'buf, IO> Session<'buf, IO>
where
    IO: Io,
{
    pub(super) fn process_received_packet(&mut self) -> Result<Option<usize>, Error<IO::Error>> {
        if !self.packet_reader.packet_available() {
            return Ok(None);
        }

        let (packet_length, packet) = match self.packet_reader.take_packet() {
            Ok(packet) => packet,
            Err(err) => {
                warn!("Failed to decode inbound packet: {:?}", err);
                self.handle_disconnect();
                return Err(err.into());
            }
        };
        match self.data.handle_packet(&mut self.runtime, packet) {
            Ok(true) => Ok(Some(packet_length)),
            Ok(false) => Ok(None),
            Err(Error::Disconnected) => {
                warn!("Disconnecting session after broker DISCONNECT");
                self.handle_disconnect();
                Err(Error::Disconnected)
            }
            Err(Error::Protocol(err))
                if matches!(
                    err,
                    ProtocolError::MalformedPacket
                        | ProtocolError::UnexpectedPacket
                        | ProtocolError::InvalidProperty
                        | ProtocolError::ProvidedClientIdTooLong
                        | ProtocolError::WrongQos
                        | ProtocolError::UnsupportedPacket
                        | ProtocolError::BadIdentifier
                        | ProtocolError::Deserialization(_)
                        | ProtocolError::Failed(ReasonCode::PacketTooLarge)
                ) =>
            {
                warn!("Disconnecting session after packet handling error");
                self.handle_disconnect();
                Err(Error::Protocol(err))
            }
            Err(Error::Protocol(err)) => Err(Error::Protocol(err)),
            Err(Error::NotReady | Error::WriteZero) => {
                unreachable!("packet handler returned local I/O state")
            }
            Err(Error::Transport(never)) => match never {},
        }
    }

    pub(super) fn decode_inbound_publish(&self, packet_length: usize) -> InboundPublish<'_> {
        let ReceivedPacket::Publish(info) =
            ReceivedPacket::from_buffer(&self.packet_reader.buffer[..packet_length])
                .expect("inbound packet must remain decodable")
        else {
            unreachable!("inbound event must be a PUBLISH");
        };
        InboundPublish::new(
            info.topic.0,
            info.payload,
            info.properties,
            info.retain,
            info.qos,
        )
    }
}
