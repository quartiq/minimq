use crate::{
    message_types::MessageType,
    packets::{ConnAck, Disconnect, Pub, PubAck, PubComp, PubRec, PubRel, SubAck},
    varint::Varint,
    ProtocolError, QoS, Retain,
};

use super::deserializer::MqttDeserializer;

use bit_field::BitField;
use core::convert::TryFrom;
use serde::Deserialize;

pub struct Packet<'a> {
    length: Varint,

    // Note: If deserialized from a split buffer, the contents of this packet may not be correct.
    // As such, we do not expose it in the public API.
    packet: ReceivedPacket<'a>,
}

impl<'a> Packet<'a> {
    pub fn len(&self) -> usize {
        self.length.len() + 1 + self.length.0 as usize
    }

    pub fn id(&self) -> Option<u16> {
        match &self.packet {
            ReceivedPacket::Publish(info) => info.packet_id,
            ReceivedPacket::PubAck(ack) => Some(ack.packet_identifier),
            ReceivedPacket::SubAck(ack) => Some(ack.packet_identifier),
            ReceivedPacket::PubRel(rel) => Some(rel.packet_id),
            ReceivedPacket::PubRec(rec) => Some(rec.packet_id),
            ReceivedPacket::PubComp(comp) => Some(comp.packet_id),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum ReceivedPacket<'a> {
    ConnAck(ConnAck<'a>),
    Publish(Pub<'a>),
    PubAck(PubAck<'a>),
    SubAck(SubAck<'a>),
    PubRel(PubRel<'a>),
    PubRec(PubRec<'a>),
    PubComp(PubComp<'a>),
    Disconnect(Disconnect<'a>),
    PingResp,
}

impl<'a> ReceivedPacket<'a> {
    pub fn from_buffer(buf: &'a [u8]) -> Result<Self, ProtocolError> {
        let mut deserializer = MqttDeserializer::new(buf);
        let mut packet = Packet::deserialize(&mut deserializer)?;

        // Remainder should never error because there is no tail data.
        let remaining_payload = deserializer.remainder().unwrap();

        // We should only have remaining payload for publish messages.
        if !remaining_payload.is_empty() {
            match &mut packet.packet {
                ReceivedPacket::Publish(publish) => {
                    publish.payload = remaining_payload;
                }
                ReceivedPacket::SubAck(suback) => {
                    suback.codes = remaining_payload;
                }
                _ => return Err(ProtocolError::MalformedPacket),
            }
        }

        Ok(packet.packet)
    }

    /// Get a received packet from a split buffer
    ///
    /// # Note
    /// Packets deserialized in this manner may not have valid binary slices. This occurs because
    /// the binary slice may span the discontinuity, and thus it's impossible to zero-copy it.
    /// Instead, the resulting binary slice will only contain the maximum amount of continuous
    /// data.
    pub fn from_split_buffer(buf: &'a [u8], tail: &'a [u8]) -> Result<Packet<'a>, ProtocolError> {
        let mut deserializer = MqttDeserializer::new_split(buf, tail);
        deserializer.truncate_on_discontinuity();
        let packet = Packet::deserialize(&mut deserializer)?;
        Ok(packet)
    }
}

struct ControlPacketVisitor;

impl<'de> serde::de::Visitor<'de> for ControlPacketVisitor {
    type Value = Packet<'de>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "MQTT Control Packet")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;

        // Note(unwraps): These unwraps should never fail - the next_element() function should be
        // always providing us some new element or an error that we return based on our
        // deserialization implementation.
        let fixed_header: u8 = seq.next_element()?.unwrap();
        let length: Varint = seq.next_element()?.unwrap();
        let packet_type = MessageType::try_from(fixed_header.get_bits(4..=7))
            .map_err(|_| A::Error::custom("Invalid MQTT control packet type"))?;

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

                let publish = Pub {
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
            MessageType::PingResp => ReceivedPacket::PingResp,
            MessageType::PubRec => ReceivedPacket::PubRec(seq.next_element()?.unwrap()),
            MessageType::PubRel => ReceivedPacket::PubRel(seq.next_element()?.unwrap()),
            MessageType::PubComp => ReceivedPacket::PubComp(seq.next_element()?.unwrap()),
            MessageType::Disconnect => ReceivedPacket::Disconnect(seq.next_element()?.unwrap()),
            _ => return Err(A::Error::custom("Unsupported message type")),
        };

        Ok(Packet { packet, length })
    }
}

impl<'de> Deserialize<'de> for Packet<'de> {
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
            0x20, // ConnAck
            0x03, // Remaining length = 3 bytes
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
    fn deserialize_split_pub() {
        let serialized_publish: [u8; 9] = [
            0x30, // Publish, no QoS
            0x05, // Remaining length
            0x00, 0x03, // Topic length (3)
            0x41, 0x42, 0x43, // Topic name: 'ABC'
            0x00, // Properties length (0)
            0x05, // Payload (0x05)
        ];

        // Split buffer
        let packet =
            ReceivedPacket::from_split_buffer(&serialized_publish[..5], &serialized_publish[5..])
                .unwrap();
        match packet.packet {
            ReceivedPacket::Publish(pub_info) => {
                // Check that the topic "ABC" was split and truncated.
                assert_eq!(pub_info.topic.0, "A");
            }
            _ => panic!("Invalid message"),
        }
    }
}
