use crate::serde_minimq::{deserializer::MqttDeserializer, varint::Varint};
use crate::ProtocolError;
use crate::{message_types::MessageType, Property};
use bit_field::BitField;
use core::convert::TryFrom;
use heapless::Vec;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct ConnAck<'a> {
    /// Indicates true if session state is being maintained by the broker.
    pub session_present: bool,

    /// A status code indicating the success status of the connection.
    pub reason_code: u8,

    /// A list of properties associated with the connection.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug, Deserialize)]
pub struct Pub<'a> {
    /// The topic that the message was received on.
    pub topic: &'a str,

    /// The properties transmitted with the publish data.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug, Deserialize)]
pub struct PubAck<'a> {
    /// Packet identifier
    pub packet_identifier: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Deserialize)]
pub struct SubAck<'a> {
    /// The identifier that the acknowledge is assocaited with.
    pub packet_identifier: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,

    code: u8,
}

impl<'a> SubAck<'a> {
    pub fn code(&self) -> u8 {
        self.code
    }
}

#[derive(Debug, Deserialize)]
pub struct PubRec<'a> {
    /// Packet identifier
    pub packet_id: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Deserialize)]
pub struct PubComp<'a> {
    /// Packet identifier
    pub packet_id: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Deserialize)]
pub struct Disconnect<'a> {
    /// The success status of the disconnection.
    pub reason_code: u8,

    /// Properties associated with the disconnection.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug)]
pub struct MqttPacket<'a> {
    pub packet: ReceivedPacket<'a>,
    pub remaining_payload: &'a [u8],
}

impl<'a> MqttPacket<'a> {
    pub fn from_buffer(buf: &'a [u8]) -> Result<Self, ProtocolError> {
        let mut deserializer = MqttDeserializer::new(buf);
        let packet = ReceivedPacket::deserialize(&mut deserializer)
            .map_err(|_| ProtocolError::MalformedPacket)?;
        let remaining_payload = deserializer.remainder();

        // We should only have remaining payload for publish messages.
        if !matches!(packet, ReceivedPacket::Publish(_)) && !remaining_payload.is_empty() {
            return Err(ProtocolError::MalformedPacket);
        }

        Ok(MqttPacket {
            packet,
            remaining_payload,
        })
    }
}

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

#[derive(Debug, Deserialize)]
pub struct Reason<'a> {
    #[serde(borrow)]
    reason: Option<ReasonData<'a>>,
}

impl<'a> Reason<'a> {
    pub fn code(&self) -> u8 {
        self.reason.as_ref().map(|data| data.code).unwrap_or(0)
    }

    pub fn properties(&self) -> &'_ [Property<'a>] {
        match &self.reason {
            Some(ReasonData { properties, .. }) => properties,
            _ => &[],
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ReasonData<'a> {
    /// Reason code
    pub code: u8,

    /// The properties transmitted with the publish data.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}

struct ControlPacketVisitor;

impl<'de> serde::de::Visitor<'de> for ControlPacketVisitor {
    type Value = ReceivedPacket<'de>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "MQTT Control Packet")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;

        let fixed_header: u8 = seq
            .next_element()?
            .ok_or_else(|| A::Error::custom("No header"))?;
        let _length: Varint = seq
            .next_element()?
            .ok_or_else(|| A::Error::custom("No length"))?;
        let packet_type = MessageType::try_from(fixed_header.get_bits(4..=7))
            .map_err(|_| A::Error::custom("Invalid MQTT control packet type"))?;

        let packet = match packet_type {
            MessageType::ConnAck => ReceivedPacket::ConnAck(
                seq.next_element()?
                    .ok_or_else(|| A::Error::custom("No ConnAck"))?,
            ),
            MessageType::Publish => ReceivedPacket::Publish(
                seq.next_element()?
                    .ok_or_else(|| A::Error::custom("No Publish"))?,
            ),
            MessageType::PubAck => ReceivedPacket::PubAck(
                seq.next_element()?
                    .ok_or_else(|| A::Error::custom("No PubAck"))?,
            ),
            MessageType::SubAck => ReceivedPacket::SubAck(
                seq.next_element()?
                    .ok_or_else(|| A::Error::custom("No SubAck"))?,
            ),
            MessageType::PingResp => ReceivedPacket::PingResp,
            MessageType::PubRec => ReceivedPacket::PubRec(
                seq.next_element()?
                    .ok_or_else(|| A::Error::custom("No PubRec"))?,
            ),
            MessageType::PubComp => ReceivedPacket::PubComp(
                seq.next_element()?
                    .ok_or_else(|| A::Error::custom("No PubComp"))?,
            ),
            MessageType::Disconnect => ReceivedPacket::Disconnect(
                seq.next_element()?
                    .ok_or_else(|| A::Error::custom("No Disconnect"))?,
            ),
            _ => return Err(A::Error::custom("Unsupported message type")),
        };

        Ok(packet)
    }
}

impl<'de> Deserialize<'de> for ReceivedPacket<'de> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_struct(
            "MqttControlPacket",
            &["fixed_header", "length", "control_packet"],
            ControlPacketVisitor,
        )
    }
}

#[cfg(test)]
mod test {
    use super::{MqttPacket, ReceivedPacket};

    #[test]
    fn deserialize_good_connack() {
        let serialized_connack: [u8; 5] = [
            0x20, 0x03, // Remaining length = 3 bytes
            0x00, // Connect acknowledge flags - bit 0 clear.
            0x00, // Connect reason code - 0 (Success)
            0x00, // Property length = 0
                  // No payload.
        ];

        let packet = MqttPacket::from_buffer(&serialized_connack).unwrap();
        match packet.packet {
            ReceivedPacket::ConnAck(conn_ack) => {
                assert_eq!(conn_ack.reason_code, 0);
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

        let packet = MqttPacket::from_buffer(&serialized_publish).unwrap();
        match packet.packet {
            ReceivedPacket::Publish(pub_info) => {
                assert_eq!(pub_info.topic, "A");
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

        let packet = MqttPacket::from_buffer(&serialized_puback).unwrap();
        match packet.packet {
            ReceivedPacket::PubAck(pub_ack) => {
                assert_eq!(pub_ack.reason.code(), 0x10);
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

        let packet = MqttPacket::from_buffer(&serialized_puback).unwrap();
        match packet.packet {
            ReceivedPacket::PubAck(pub_ack) => {
                assert_eq!(pub_ack.packet_identifier, 6);
                assert_eq!(pub_ack.reason.code(), 0);
                assert!(pub_ack.reason.reason.is_none());
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

        let packet = MqttPacket::from_buffer(&serialized_suback).unwrap();
        match packet.packet {
            ReceivedPacket::SubAck(sub_ack) => {
                assert_eq!(sub_ack.code(), 2);
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

        let packet = MqttPacket::from_buffer(&serialized_ping_req).unwrap();
        match packet.packet {
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
            0x16, // Response Code
            0x00, // Properties length
        ];
        let packet = MqttPacket::from_buffer(&serialized_pubcomp).unwrap();
        match packet.packet {
            ReceivedPacket::PubComp(comp) => {
                assert_eq!(comp.packet_id, 5);
                assert_eq!(comp.reason.code(), 0x16);
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
        let packet = MqttPacket::from_buffer(&serialized_pubcomp).unwrap();
        match packet.packet {
            ReceivedPacket::PubComp(comp) => {
                assert_eq!(comp.packet_id, 5);
                assert_eq!(comp.reason.code(), 0);
                assert!(comp.reason.reason.is_none());
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
        let packet = MqttPacket::from_buffer(&serialized_pubrec).unwrap();
        match packet.packet {
            ReceivedPacket::PubRec(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason.code(), 0x10);
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
        let packet = MqttPacket::from_buffer(&serialized_pubrec).unwrap();
        match packet.packet {
            ReceivedPacket::PubRec(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason.code(), 0);
                assert!(rec.reason.reason.is_none());
            }
            _ => panic!("Invalid message"),
        }
    }
}
