use crate::{
    ProtocolError, QoS, Retain,
    message_types::MessageType,
    packets::{ConnAck, Disconnect, Pub, PubAck, PubComp, PubRec, PubRel, SubAck, UnsubAck},
    varint::Varint,
};

use super::deserializer::MqttDeserializer;
use crate::{trace, warn};

use bit_field::BitField;
use core::convert::TryFrom;
use serde::Deserialize;

#[derive(Debug)]
pub enum ReceivedPacket<'a> {
    ConnAck(ConnAck<'a>),
    Publish(Pub<'a, &'a [u8]>),
    PubAck(PubAck<'a>),
    SubAck(SubAck<'a>),
    UnsubAck(UnsubAck<'a>),
    PubRel(PubRel<'a>),
    PubRec(PubRec<'a>),
    PubComp(PubComp<'a>),
    #[allow(dead_code)]
    Disconnect(Disconnect<'a>),
    PingResp,
}

impl<'a> ReceivedPacket<'a> {
    pub fn kind(&self) -> &'static str {
        match self {
            ReceivedPacket::ConnAck(_) => "CONNACK",
            ReceivedPacket::Publish(_) => "PUBLISH",
            ReceivedPacket::PubAck(_) => "PUBACK",
            ReceivedPacket::SubAck(_) => "SUBACK",
            ReceivedPacket::UnsubAck(_) => "UNSUBACK",
            ReceivedPacket::PubRel(_) => "PUBREL",
            ReceivedPacket::PubRec(_) => "PUBREC",
            ReceivedPacket::PubComp(_) => "PUBCOMP",
            ReceivedPacket::Disconnect(_) => "DISCONNECT",
            ReceivedPacket::PingResp => "PINGRESP",
        }
    }

    pub fn from_buffer(buf: &'a [u8]) -> Result<Self, ProtocolError> {
        trace!("Parsing received packet buffer of {} bytes", buf.len());
        let mut deserializer = MqttDeserializer::new(buf);
        let mut packet = ReceivedPacket::deserialize(&mut deserializer)?;

        let remaining_payload = deserializer.remainder();

        // We should only have remaining payload for publish messages.
        if !remaining_payload.is_empty() {
            match &mut packet {
                ReceivedPacket::Publish(publish) => {
                    publish.payload = remaining_payload;
                }
                ReceivedPacket::SubAck(suback) => {
                    suback.codes = remaining_payload;
                }
                ReceivedPacket::UnsubAck(unsuback) => {
                    unsuback.codes = remaining_payload;
                }
                _ => {
                    warn!(
                        "Unexpected trailing payload of {} bytes for non-payload packet",
                        remaining_payload.len()
                    );
                    return Err(ProtocolError::MalformedPacket);
                }
            }
        }

        trace!("Parsed inbound packet kind={}", packet.kind());
        Ok(packet)
    }
}

struct ControlPacketVisitor;

impl<'de> serde::de::Visitor<'de> for ControlPacketVisitor {
    type Value = ReceivedPacket<'de>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "MQTT Control Packet")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;

        // Note(unwraps): These unwraps should never fail - the next_element() function should be
        // always providing us some new element or an error that we return based on our
        // deserialization implementation.
        let fixed_header: u8 = seq.next_element()?.unwrap();
        let _length: Varint = seq.next_element()?.unwrap();
        let packet_type = MessageType::try_from(fixed_header.get_bits(4..=7))
            .map_err(|_| A::Error::custom("Invalid MQTT control packet type"))?;
        let flags = fixed_header.get_bits(0..=3);
        trace!(
            "Received fixed header: type={:?} flags={:#b} remaining_len={:?}",
            packet_type, flags, _length
        );

        let valid_flags = match packet_type {
            MessageType::Publish => true,
            MessageType::PubRel => flags == 0b0010,
            MessageType::ConnAck
            | MessageType::PubAck
            | MessageType::PubRec
            | MessageType::PubComp
            | MessageType::SubAck
            | MessageType::UnsubAck
            | MessageType::PingResp
            | MessageType::Disconnect => flags == 0,
            _ => true,
        };
        if !valid_flags {
            warn!(
                "Rejecting packet {:?} due to invalid flags {:#b}",
                packet_type, flags
            );
            return Err(A::Error::custom("Invalid MQTT control packet flags"));
        }

        let packet = match packet_type {
            MessageType::ConnAck => ReceivedPacket::ConnAck(seq.next_element()?.unwrap()),
            MessageType::Publish => {
                let qos = QoS::try_from(fixed_header.get_bits(1..=2))
                    .map_err(|_| A::Error::custom("Bad QoS field"))?;

                let topic = seq.next_element()?.unwrap();
                let packet_id = if qos > QoS::AtMostOnce {
                    Some(seq.next_element()?.unwrap())
                } else {
                    None
                };

                let properties = seq.next_element()?.unwrap();

                let publish: Pub<'_, &[u8]> = Pub {
                    topic,
                    packet_id,
                    properties,
                    payload: &[],
                    retain: if fixed_header.get_bit(0) {
                        Retain::Retained
                    } else {
                        Retain::NotRetained
                    },
                    dup: fixed_header.get_bit(3),
                    qos,
                };

                ReceivedPacket::Publish(publish)
            }
            MessageType::PubAck => ReceivedPacket::PubAck(seq.next_element()?.unwrap()),
            MessageType::SubAck => ReceivedPacket::SubAck(seq.next_element()?.unwrap()),
            MessageType::UnsubAck => ReceivedPacket::UnsubAck(seq.next_element()?.unwrap()),
            MessageType::PingResp => ReceivedPacket::PingResp,
            MessageType::PubRec => ReceivedPacket::PubRec(seq.next_element()?.unwrap()),
            MessageType::PubRel => ReceivedPacket::PubRel(seq.next_element()?.unwrap()),
            MessageType::PubComp => ReceivedPacket::PubComp(seq.next_element()?.unwrap()),
            MessageType::Disconnect => ReceivedPacket::Disconnect(seq.next_element()?.unwrap()),
            _ => return Err(A::Error::custom("Unsupported message type")),
        };

        Ok(packet)
    }
}

impl<'de> Deserialize<'de> for ReceivedPacket<'de> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // Deserialize the (fixed_header, length, control_packet | (topic, packet_id, properties)),
        // which corresponds to a maximum of 5 elements.
        deserializer.deserialize_tuple(5, ControlPacketVisitor)
    }
}

#[cfg(test)]
mod test {
    use super::ReceivedPacket;
    use crate::reason_codes::ReasonCode;

    #[test]
    fn deserialize_good_connack() {
        env_logger::init();
        let serialized_connack: [u8; 5] = [
            0x20, 0x03, // Remaining length = 3 bytes
            0x00, // Connect acknowledge flags - bit 0 clear.
            0x00, // Connect reason code - 0 (Success)
            0x00, // Property length = 0
                  // No payload.
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_connack).unwrap();
        match packet {
            ReceivedPacket::ConnAck(conn_ack) => {
                assert_eq!(conn_ack.reason_code, ReasonCode::Success);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_publish() {
        let serialized_publish: [u8; 7] = [
            0x30, // Publish, no QoS
            0x05, // Remaining length
            0x00, 0x01, // Topic length (1)
            0x41, // Topic name: 'A'
            0x00, // Properties length
            0x05, // Payload
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_publish).unwrap();
        match packet {
            ReceivedPacket::Publish(pub_info) => {
                assert_eq!(pub_info.topic.0, "A");
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_puback() {
        let serialized_puback: [u8; 6] = [
            0x40, // PubAck
            0x04, // Remaining length
            0x00, 0x05, // Identifier
            0x10, // Response Code
            0x00, // Properties length
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_puback).unwrap();
        match packet {
            ReceivedPacket::PubAck(pub_ack) => {
                assert_eq!(pub_ack.reason.code(), ReasonCode::NoMatchingSubscribers);
                assert_eq!(pub_ack.packet_identifier, 5);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_puback_without_reason() {
        let serialized_puback: [u8; 4] = [
            0x40, // PubAck
            0x02, // Remaining length
            0x00, 0x06, // Identifier
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_puback).unwrap();
        match packet {
            ReceivedPacket::PubAck(pub_ack) => {
                assert_eq!(pub_ack.packet_identifier, 6);
                assert_eq!(pub_ack.reason.code(), ReasonCode::Success);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_puback_without_properties() {
        let serialized_puback: [u8; 5] = [
            0x40, // PubAck
            0x03, // Remaining length
            0x00, 0x06, // Identifier
            0x10, // ReasonCode
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_puback).unwrap();
        match packet {
            ReceivedPacket::PubAck(pub_ack) => {
                assert_eq!(pub_ack.packet_identifier, 6);
                assert_eq!(pub_ack.reason.code(), ReasonCode::NoMatchingSubscribers);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_suback() {
        let serialized_suback: [u8; 6] = [
            0x90, // SubAck
            0x04, // Remaining length
            0x00, 0x05, // Identifier
            0x00, // Properties length
            0x02, // Response Code
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_suback).unwrap();
        match packet {
            ReceivedPacket::SubAck(sub_ack) => {
                assert_eq!(sub_ack.codes.len(), 1);
                assert_eq!(ReasonCode::from(sub_ack.codes[0]), ReasonCode::GrantedQos2);
                assert_eq!(sub_ack.packet_identifier, 5);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_unsuback() {
        let serialized_unsuback: [u8; 6] = [
            0xB0, // UnsubAck
            0x04, // Remaining length
            0x00, 0x05, // Identifier
            0x00, // Properties length
            0x11, // Response Code
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_unsuback).unwrap();
        match packet {
            ReceivedPacket::UnsubAck(unsub_ack) => {
                assert_eq!(unsub_ack.codes.len(), 1);
                assert_eq!(
                    ReasonCode::from(unsub_ack.codes[0]),
                    ReasonCode::NoSubscriptionExisted
                );
                assert_eq!(unsub_ack.packet_identifier, 5);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_ping_resp() {
        let serialized_ping_req: [u8; 2] = [
            0xd0, // Ping resp
            0x00, // Remaining length (0)
        ];

        let packet = ReceivedPacket::from_buffer(&serialized_ping_req).unwrap();
        match packet {
            ReceivedPacket::PingResp => {}
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn reject_ping_resp_with_invalid_flags() {
        let serialized_ping_resp: [u8; 2] = [
            0xd1, // Ping resp with invalid flags
            0x00,
        ];

        assert!(matches!(
            ReceivedPacket::from_buffer(&serialized_ping_resp),
            Err(crate::ProtocolError::Deserialization(
                crate::DeError::Custom
            ))
        ));
    }

    #[test]
    fn reject_pubrel_with_invalid_flags() {
        let serialized_pubrel: [u8; 4] = [
            0x60, // PubRel with invalid flags
            0x02, 0x00, 0x05,
        ];

        assert!(matches!(
            ReceivedPacket::from_buffer(&serialized_pubrel),
            Err(crate::ProtocolError::Deserialization(
                crate::DeError::Custom
            ))
        ));
    }

    #[test]
    fn deserialize_good_pubcomp() {
        let serialized_pubcomp: [u8; 6] = [
            7 << 4, // PubComp
            0x04,   // Remaining length
            0x00,
            0x05, // Identifier
            0x92, // Response Code
            0x00, // Properties length
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_pubcomp).unwrap();
        match packet {
            ReceivedPacket::PubComp(comp) => {
                assert_eq!(comp.packet_id, 5);
                assert_eq!(comp.reason.code(), ReasonCode::PacketIdNotFound);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_short_pubcomp() {
        let serialized_pubcomp: [u8; 4] = [
            7 << 4, // PubComp
            0x02,   // Remaining length
            0x00,
            0x05, // Identifier
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_pubcomp).unwrap();
        match packet {
            ReceivedPacket::PubComp(comp) => {
                assert_eq!(comp.packet_id, 5);
                assert_eq!(comp.reason.code(), ReasonCode::Success);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_pubrec() {
        let serialized_pubrec: [u8; 6] = [
            5 << 4, // PubRec
            0x04,   // Remaining length
            0x00,
            0x05, // Identifier
            0x10, // Response Code
            0x00, // Properties length
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_pubrec).unwrap();
        match packet {
            ReceivedPacket::PubRec(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason.code(), ReasonCode::NoMatchingSubscribers);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_short_pubrec() {
        let serialized_pubrec: [u8; 4] = [
            5 << 4, // PubRec
            0x02,   // Remaining length
            0x00,
            0x05, // Identifier
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_pubrec).unwrap();
        match packet {
            ReceivedPacket::PubRec(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason.code(), ReasonCode::Success);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_pubrel() {
        let serialized_pubrel: [u8; 6] = [
            6 << 4 | 0b10, // PubRec
            0x04,          // Remaining length
            0x00,
            0x05, // Identifier
            0x10, // Response Code
            0x00, // Properties length
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_pubrel).unwrap();
        match packet {
            ReceivedPacket::PubRel(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason.code(), ReasonCode::NoMatchingSubscribers);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_short_pubrel() {
        let serialized_pubrel: [u8; 4] = [
            6 << 4 | 0b10, // PubRec
            0x02,          // Remaining length
            0x00,
            0x05, // Identifier
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_pubrel).unwrap();
        match packet {
            ReceivedPacket::PubRel(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason.code(), ReasonCode::Success);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_disconnect_with_reason_only() {
        let serialized_disconnect: [u8; 3] = [
            14 << 4, // Disconnect
            0x01,    // Remaining length
            0x82,    // Protocol Error
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_disconnect).unwrap();
        match packet {
            ReceivedPacket::Disconnect(disconnect) => {
                assert_eq!(disconnect.reason_code, ReasonCode::ProtocolError);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_disconnect_without_reason() {
        let serialized_disconnect: [u8; 2] = [
            14 << 4, // Disconnect
            0x00,    // Remaining length
        ];
        let packet = ReceivedPacket::from_buffer(&serialized_disconnect).unwrap();
        match packet {
            ReceivedPacket::Disconnect(disconnect) => {
                assert_eq!(disconnect.reason_code, ReasonCode::Success);
            }
            _ => panic!("Invalid message"),
        }
    }
}
