use crate::{
    message_types::MessageType,
    packets::{ConnAck, Disconnect, Pub, PubAck, PubComp, PubRec, SubAck},
    varint::Varint,
    ProtocolError, QoS, Retain,
};

use super::deserializer::MqttDeserializer;

use bit_field::BitField;
use core::convert::TryFrom;
use serde::Deserialize;

#[derive(Debug)]
pub enum ReceivedPacket<'a> {
    ConnAck(ConnAck<'a>),
    Publish(Pub<'a>),
    PubAck(PubAck<'a>),
    SubAck(SubAck<'a>),
    PubRec(PubRec<'a>),
    PubComp(PubComp<'a>),
    Disconnect(Disconnect<'a>),
    PingResp,
}

impl<'a> ReceivedPacket<'a> {
    pub fn from_buffer(buf: &'a [u8]) -> Result<Self, ProtocolError> {
        let mut deserializer = MqttDeserializer::new(buf);
        let mut packet = ReceivedPacket::deserialize(&mut deserializer)?;

        let remaining_payload = deserializer.remainder();

        // We should only have remaining payload for publish messages.
        if !remaining_payload.is_empty() {
            match &mut packet {
                ReceivedPacket::Publish(publish) => {
                    publish.payload = remaining_payload;
                }
                _ => return Err(ProtocolError::MalformedPacket),
            }
        }

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
        env_logger::init();
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
                assert_eq!(pub_ack.reason.properties().len(), 0);
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
                assert_eq!(sub_ack.code, ReasonCode::GrantedQos2);
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
                assert_eq!(comp.reason.properties().len(), 0);
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
                assert_eq!(rec.reason.properties().len(), 0);
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
}
