use crate::de::received_packet::ReceivedPacket;
use crate::packets::{PubAck, PubComp, PubRec, PubRel};
use crate::{Error, Property, ProtocolError, QoS, ReasonCode, debug};
use core::convert::TryFrom;
use embassy_time::{Duration, Instant};
use embedded_io_async::{ErrorType, Read, Write};
use heapless::String;

use super::InboundPublish;
use super::core::{ConnectionState, RuntimeState, SessionData};
use super::outbound::write_control_packet;

pub(super) struct PacketContext<'a, 'buf> {
    pub(super) client_id: &'a mut String<64>,
    pub(super) session: &'a mut SessionData<'buf>,
    pub(super) runtime: &'a mut RuntimeState,
}

impl PacketContext<'_, '_> {
    pub(super) fn mark_disconnected(&mut self) {
        self.session.outbound.arm_replay();
        self.runtime.disconnect();
    }
}

pub(super) async fn handle_packet<'pkt, 'state, C>(
    cx: &mut PacketContext<'_, 'state>,
    connection: &mut C,
    packet: ReceivedPacket<'pkt>,
    now: Instant,
) -> Result<Option<InboundPublish<'pkt>>, Error>
where
    C: Read + Write + ErrorType,
    C::Error: embedded_io_async::Error,
{
    match packet {
        ReceivedPacket::ConnAck(ack) => {
            ack.reason_code.as_result()?;
            if !ack.session_present {
                cx.session.reset();
                cx.runtime.pending_subscriptions.clear();
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

            cx.runtime.state = ConnectionState::Active;
            cx.session.register_connected();
            cx.runtime.next_ping = Some(now + cx.runtime.keepalive_interval / 2);
            cx.runtime.ping_timeout = None;
            if cx.session.take_reset() {
                return Err(Error::SessionReset);
            }
        }
        ReceivedPacket::SubAck(ack) => {
            let index = cx
                .runtime
                .pending_subscriptions
                .iter()
                .position(|id| *id == ack.packet_identifier)
                .ok_or(ProtocolError::BadIdentifier)?;
            cx.runtime.pending_subscriptions.swap_remove(index);
            for &code in ack.codes {
                ReasonCode::from(code).as_result()?;
            }
        }
        ReceivedPacket::PingResp => {
            cx.runtime.ping_timeout = None;
        }
        ReceivedPacket::PubAck(ack) => {
            cx.runtime.send_quota = cx
                .runtime
                .send_quota
                .saturating_add(1)
                .min(cx.runtime.max_send_quota);
            cx.session.outbound.ack_publish(ack.packet_identifier)?;
        }
        ReceivedPacket::PubRec(rec) => {
            rec.reason.code().as_result()?;
            cx.runtime.send_quota = cx
                .runtime
                .send_quota
                .saturating_add(1)
                .min(cx.runtime.max_send_quota);
            cx.session.outbound.ack_publish(rec.packet_id)?;
            cx.session
                .outbound
                .queue_release(rec.packet_id, ReasonCode::Success)?;
            write_control_packet(
                connection,
                &PubRel {
                    packet_id: rec.packet_id,
                    reason: ReasonCode::Success.into(),
                },
            )
            .await?;
        }
        ReceivedPacket::PubComp(comp) => {
            cx.session.outbound.ack_release(comp.packet_id)?;
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
            write_control_packet(
                connection,
                &PubComp {
                    packet_id: rel.packet_id,
                    reason: reason.into(),
                },
            )
            .await?;
        }
        ReceivedPacket::Publish(info) => {
            let retain = info.retain;
            let qos = info.qos;
            match info.qos {
                QoS::AtMostOnce => {}
                QoS::AtLeastOnce => {
                    let packet_id = info.packet_id.ok_or(ProtocolError::MalformedPacket)?;
                    let reason = if cx.session.pending_server_packet_ids.contains(&packet_id) {
                        ReasonCode::PacketIdInUse
                    } else {
                        ReasonCode::Success
                    };
                    write_control_packet(
                        connection,
                        &PubAck {
                            packet_identifier: packet_id,
                            reason: reason.into(),
                        },
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
                    write_control_packet(
                        connection,
                        &PubRec {
                            packet_id,
                            reason: reason.into(),
                        },
                    )
                    .await?;
                    if duplicate || !reason.success() {
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
            debug!("Received broker DISCONNECT");
            cx.mark_disconnected();
        }
    }

    Ok(None)
}
