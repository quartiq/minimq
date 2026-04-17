use crate::de::received_packet::ReceivedPacket;
use crate::packets::{PubAck, PubComp, PubRec};
use crate::{Error, Property, ProtocolError, QoS, ReasonCode, debug, info, trace, warn};
use core::convert::TryFrom;
use embassy_time::{Duration, Instant};
use heapless::String;

use super::core::{ConnectionState, RuntimeState, SessionData};
use super::outbound::write_control_packet;
use super::{InboundPublish, Io};

pub(super) struct PacketContext<'a, 'buf> {
    pub(super) client_id: &'a mut String<64>,
    pub(super) session: &'a mut SessionData<'buf>,
    pub(super) runtime: &'a mut RuntimeState,
}

pub(super) async fn handle_packet<'pkt, 'state, C: Io>(
    cx: &mut PacketContext<'_, 'state>,
    connection: &mut C,
    packet: ReceivedPacket<'pkt>,
    now: Instant,
) -> Result<Option<InboundPublish<'pkt>>, Error> {
    match packet {
        ReceivedPacket::ConnAck(ack) => {
            if let Err(err) = ack.reason_code.as_result() {
                warn!("Broker rejected CONNECT with reason {:?}", ack.reason_code);
                cx.runtime.disconnect();
                return Err(err.into());
            }
            cx.runtime.session_resumed = ack.session_present;
            if !ack.session_present {
                debug!("Broker started a fresh session; resetting local session state");
                cx.session.reset();
            }

            cx.runtime.send_quota = cx.session.outbound.max_inflight();
            cx.runtime.max_send_quota = cx.session.outbound.max_inflight();
            cx.runtime.max_qos = None;
            cx.runtime.maximum_packet_size = None;

            for property in ack.properties.into_iter() {
                match property? {
                    Property::MaximumPacketSize(size) => {
                        cx.runtime.maximum_packet_size = Some(size)
                    }
                    Property::AssignedClientIdentifier(id) => {
                        *cx.client_id = String::try_from(id.0)
                            .map_err(|_| ProtocolError::ProvidedClientIdTooLong)?;
                    }
                    Property::ServerKeepAlive(keepalive) => {
                        cx.runtime.keepalive_interval = Duration::from_secs(keepalive as u64);
                    }
                    Property::ReceiveMaximum(max) => {
                        let local = cx.session.outbound.max_inflight();
                        cx.runtime.send_quota = max.min(local);
                        cx.runtime.max_send_quota = max.min(local);
                    }
                    Property::MaximumQoS(max) => {
                        cx.runtime.max_qos =
                            Some(QoS::try_from(max).map_err(|_| ProtocolError::WrongQos)?);
                    }
                    _ => {}
                }
            }

            debug!(
                "Activated session state resumed={} send_quota={}/{} max_qos={:?} max_packet_size={:?}",
                ack.session_present,
                cx.runtime.send_quota,
                cx.runtime.max_send_quota,
                cx.runtime.max_qos,
                cx.runtime.maximum_packet_size
            );

            cx.runtime.state = ConnectionState::Active;
            cx.session.register_connected();
            cx.runtime.next_ping = Some(now + cx.runtime.keepalive_interval / 2);
            cx.runtime.ping_timeout = None;
            if ack.session_present {
                info!("Connected and resumed existing broker session");
            } else {
                info!("Connected with a fresh broker session");
            }
        }
        ReceivedPacket::SubAck(ack) => {
            if !cx.session.outbound.ack_packet(ack.packet_identifier) {
                debug!(
                    "Ignoring stale SUBACK for packet id {}",
                    ack.packet_identifier
                );
                return Ok(None);
            }
            debug!("Processed SUBACK packet_id={}", ack.packet_identifier);
            for &code in ack.codes {
                ReasonCode::from(code).as_result()?;
            }
        }
        ReceivedPacket::UnsubAck(ack) => {
            if !cx.session.outbound.ack_packet(ack.packet_identifier) {
                debug!(
                    "Ignoring stale UNSUBACK for packet id {}",
                    ack.packet_identifier
                );
                return Ok(None);
            }
            debug!("Processed UNSUBACK packet_id={}", ack.packet_identifier);
            for &code in ack.codes {
                ReasonCode::from(code).as_result()?;
            }
        }
        ReceivedPacket::PingResp => {
            trace!("Received PINGRESP");
            cx.runtime.ping_timeout = None;
        }
        ReceivedPacket::PubAck(ack) => {
            if !cx.session.outbound.ack_packet(ack.packet_identifier) {
                debug!(
                    "Ignoring stale PUBACK for packet id {}",
                    ack.packet_identifier
                );
                return Ok(None);
            }
            cx.runtime.send_quota = cx
                .runtime
                .send_quota
                .saturating_add(1)
                .min(cx.runtime.max_send_quota);
            debug!(
                "Processed PUBACK packet_id={} send_quota={}",
                ack.packet_identifier, cx.runtime.send_quota
            );
            ack.reason.code().as_result()?;
        }
        ReceivedPacket::PubRec(rec) => {
            let queue_release = match cx.session.outbound.ack_packet(rec.packet_id) {
                true => {
                    cx.runtime.send_quota = cx
                        .runtime
                        .send_quota
                        .saturating_add(1)
                        .min(cx.runtime.max_send_quota);
                    debug!(
                        "Processed PUBREC packet_id={} send_quota={}",
                        rec.packet_id, cx.runtime.send_quota
                    );
                    true
                }
                false if cx.session.outbound.has_pending_release(rec.packet_id) => {
                    debug!(
                        "Replaying PUBREL after stale PUBREC for packet id {}",
                        rec.packet_id
                    );
                    false
                }
                false => {
                    debug!("Ignoring stale PUBREC for packet id {}", rec.packet_id);
                    return Ok(None);
                }
            };
            rec.reason.code().as_result()?;
            if queue_release {
                cx.session
                    .outbound
                    .queue_release(rec.packet_id, ReasonCode::Success)?;
                debug!("Queued PUBREL for packet_id={}", rec.packet_id);
            }
        }
        ReceivedPacket::PubComp(comp) => {
            if !cx.session.outbound.ack_release(comp.packet_id) {
                debug!("Ignoring stale PUBCOMP for packet id {}", comp.packet_id);
                return Ok(None);
            }
            debug!("Processed PUBCOMP packet_id={}", comp.packet_id);
            comp.reason.code().as_result()?;
        }
        ReceivedPacket::PubRel(rel) => {
            let reason = if let Some(index) = cx
                .session
                .pending_server_packet_ids
                .iter()
                .position(|id| *id == rel.packet_id)
            {
                cx.session.pending_server_packet_ids.swap_remove(index);
                ReasonCode::Success
            } else {
                ReasonCode::PacketIdNotFound
            };
            debug!(
                "Replying to inbound PUBREL packet_id={} reason={:?} pending_inbound_qos2={}",
                rel.packet_id,
                reason,
                cx.session.pending_server_packet_ids.len()
            );
            write_control_packet(
                connection,
                &PubComp {
                    packet_id: rel.packet_id,
                    reason: reason.into(),
                },
                cx.runtime.maximum_packet_size,
            )
            .await?;
        }
        ReceivedPacket::Publish(info) => {
            let retain = info.retain;
            let qos = info.qos;
            debug!(
                "Handling inbound PUBLISH packet_id={:?} topic={} qos={:?} retain={:?} payload_len={}",
                info.packet_id,
                info.topic.0,
                qos,
                retain,
                info.payload.len()
            );
            match info.qos {
                QoS::AtMostOnce => {}
                QoS::AtLeastOnce => {
                    let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                    let reason = if cx.session.pending_server_packet_ids.contains(&packet_id) {
                        ReasonCode::PacketIdInUse
                    } else {
                        ReasonCode::Success
                    };
                    trace!(
                        "Replying to inbound QoS1 PUBLISH packet_id={} with PUBACK {:?}",
                        packet_id, reason
                    );
                    write_control_packet(
                        connection,
                        &PubAck {
                            packet_identifier: packet_id,
                            reason: reason.into(),
                        },
                        cx.runtime.maximum_packet_size,
                    )
                    .await?;
                }
                QoS::ExactlyOnce => {
                    let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                    let duplicate = cx.session.pending_server_packet_ids.contains(&packet_id);
                    let reason = if !duplicate {
                        cx.session
                            .pending_server_packet_ids
                            .push(packet_id)
                            .map(|_| ReasonCode::Success)
                            .unwrap_or(ReasonCode::ReceiveMaxExceeded)
                    } else {
                        ReasonCode::Success
                    };
                    trace!(
                        "Replying to inbound QoS2 PUBLISH packet_id={} duplicate={} with PUBREC {:?}",
                        packet_id, duplicate, reason
                    );
                    write_control_packet(
                        connection,
                        &PubRec {
                            packet_id,
                            reason: reason.into(),
                        },
                        cx.runtime.maximum_packet_size,
                    )
                    .await?;
                    if duplicate || !reason.success() {
                        debug!(
                            "Ignoring inbound QoS2 PUBLISH after PUBREC packet_id={} duplicate={} reason={:?}",
                            packet_id, duplicate, reason
                        );
                        return Ok(None);
                    }
                }
            }

            cx.runtime.next_ping = Some(now + cx.runtime.keepalive_interval / 2);
            return Ok(Some(InboundPublish {
                topic: info.topic.0,
                payload: info.payload,
                properties: info.properties,
                retain,
                qos,
            }));
        }
        ReceivedPacket::Disconnect(_) => {
            info!("Received broker DISCONNECT");
            cx.session.outbound.arm_replay();
            cx.runtime.disconnect();
        }
    }

    Ok(None)
}
