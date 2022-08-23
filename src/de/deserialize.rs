use crate::{message_types::MessageType, Property, ProtocolError as Error};
use bit_field::BitField;
use heapless::Vec;

use super::packet_parser::PacketParser;

#[derive(Debug)]
pub struct ConnAck<'a> {
    /// Indicates true if session state is being maintained by the broker.
    pub session_present: bool,

    /// A status code indicating the success status of the connection.
    pub reason_code: u8,

    /// A list of properties associated with the connection.
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug)]
pub struct Pub<'a> {
    /// The topic that the message was received on.
    pub topic: &'a str,

    /// The properties transmitted with the publish data.
    pub properties: Vec<Property<'a>, 8>,

    /// The payload of the message.
    pub payload: &'a [u8],
}

#[derive(Debug)]
pub struct PubAck<'a> {
    /// Packet identifier
    pub packet_identifier: u16,

    /// Reason code
    pub reason: u8,

    /// The properties transmitted with the publish data.
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug)]
pub struct SubAck<'a> {
    /// The identifier that the acknowledge is assocaited with.
    pub packet_identifier: u16,

    /// The success status of the subscription request.
    pub reason_code: u8,

    /// A list of properties associated with the subscription.
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug)]
pub struct PubRec<'a> {
    /// Packet identifier
    pub packet_id: u16,

    /// The success status of the publication reception.
    pub reason_code: u8,

    /// Properties associated with the reception result.
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug)]
pub struct PubComp<'a> {
    /// Packet identifier
    pub packet_id: u16,

    /// The success status of the publication completion.
    pub reason_code: u8,

    /// Properties associated with the completion.
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug)]
pub struct Disconnect<'a> {
    /// The success status of the disconnection.
    pub reason_code: u8,

    /// Properties associated with the disconnection.
    pub properties: Vec<Property<'a>, 8>,
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

impl<'a> ReceivedPacket<'a> {
    /// Parse a message out of a `PacketParser` into a validated MQTT control message.
    ///
    /// # Args
    /// * `packet_reader` - The reader to parse the message out of.
    ///
    /// # Returns
    /// A packet describing the received content.
    pub(crate) fn parse_message<'reader: 'a>(
        packet_reader: &'reader PacketParser<'_>,
    ) -> Result<ReceivedPacket<'a>, Error> {
        let (message_type, flags, remaining_length) = packet_reader.read_fixed_header()?;

        // Validate packet length.
        if remaining_length != packet_reader.len()? {
            return Err(Error::MalformedPacket);
        }

        match message_type {
            MessageType::ConnAck => {
                if flags != 0 {
                    return Err(Error::MalformedPacket);
                }

                Ok(ReceivedPacket::ConnAck(parse_connack(packet_reader)?))
            }

            MessageType::Publish => Ok(ReceivedPacket::Publish(parse_publish(packet_reader)?)),

            MessageType::PubAck => Ok(ReceivedPacket::PubAck(parse_puback(packet_reader)?)),

            MessageType::SubAck => {
                if flags != 0 {
                    return Err(Error::MalformedPacket);
                }

                Ok(ReceivedPacket::SubAck(parse_suback(packet_reader)?))
            }

            MessageType::PingResp => {
                if flags != 0 || remaining_length != 0 {
                    return Err(Error::MalformedPacket);
                }

                Ok(ReceivedPacket::PingResp)
            }

            MessageType::PubRec => Ok(ReceivedPacket::PubRec(parse_pubrec(packet_reader)?)),
            MessageType::PubComp => Ok(ReceivedPacket::PubComp(parse_pubcomp(packet_reader)?)),
            MessageType::Disconnect => {
                Ok(ReceivedPacket::Disconnect(parse_disconnect(packet_reader)?))
            }

            _ => Err(Error::UnsupportedPacket),
        }
    }
}

fn parse_connack<'a>(p: &'a PacketParser<'_>) -> Result<ConnAck<'a>, Error> {
    // Read the connect acknowledgement flags.
    let flags = p.read_u8()?;
    if flags != 0 && flags != 1 {
        return Err(Error::MalformedPacket);
    }

    let reason_code = p.read_u8()?;

    // Parse properties.
    let properties = p.read_properties()?;

    // TODO: Validate properties associated with this message.

    Ok(ConnAck {
        reason_code,
        session_present: flags.get_bit(0),
        properties,
    })
}

fn parse_publish<'a>(p: &'a PacketParser<'_>) -> Result<Pub<'a>, Error> {
    let topic = p.read_utf8_string()?;

    let properties = p.read_properties()?;
    // TODO: Validate properties associated with this message.

    let payload = p.payload()?;

    Ok(Pub {
        topic,
        properties,
        payload,
    })
}

fn parse_puback<'a>(p: &'a PacketParser<'_>) -> Result<PubAck<'a>, Error> {
    let id = p.read_u16()?;
    // If there's no available data, the reason code is zero and the properties are empty.
    if p.len()? == 0 {
        return Ok(PubAck {
            packet_identifier: id,
            reason: 0x00,
            properties: Vec::new(),
        });
    }

    let reason = p.read_u8()?;
    let properties = p.read_properties()?;

    Ok(PubAck {
        packet_identifier: id,
        reason,
        properties,
    })
}

fn parse_suback<'a>(p: &'a PacketParser<'_>) -> Result<SubAck<'a>, Error> {
    // Read the variable length header.
    let id = p.read_u16()?;

    // Parse all properties in the SubAck.
    let properties = p.read_properties()?;
    // TODO: Validate properties associated with this message.

    // Read the final payload, which contains the reason code.
    let reason_code = p.read_u8()?;

    Ok(SubAck {
        packet_identifier: id,
        reason_code,
        properties,
    })
}

fn parse_pubrec<'a>(p: &'a PacketParser<'_>) -> Result<PubRec<'a>, Error> {
    let id = p.read_u16()?;

    // Reason code and properties are both optionally present.
    if p.len()? == 0 {
        return Ok(PubRec {
            packet_id: id,
            reason_code: 0,
            properties: Vec::new(),
        });
    }

    let reason_code = p.read_u8()?;
    let properties = p.read_properties()?;

    Ok(PubRec {
        packet_id: id,
        reason_code,
        properties,
    })
}

fn parse_pubcomp<'a>(p: &'a PacketParser<'_>) -> Result<PubComp<'a>, Error> {
    let id = p.read_u16()?;

    // Reason code and properties are both optionally present.
    if p.len()? == 0 {
        return Ok(PubComp {
            packet_id: id,
            reason_code: 0,
            properties: Vec::new(),
        });
    }

    let reason_code = p.read_u8()?;
    let properties = p.read_properties()?;

    Ok(PubComp {
        packet_id: id,
        reason_code,
        properties,
    })
}

fn parse_disconnect<'a>(p: &'a PacketParser<'_>) -> Result<Disconnect<'a>, Error> {
    let reason_code = p.read_u8()?;
    let properties = p.read_properties()?;
    Ok(Disconnect {
        reason_code,
        properties,
    })
}

#[cfg(test)]
mod test {
    use super::{PacketParser, ReceivedPacket};

    #[test]
    fn deserialize_good_connack() {
        let serialized_connack: [u8; 5] = [
            0x20, 0x03, // Remaining length = 3 bytes
            0x00, // Connect acknowledge flags - bit 0 clear.
            0x00, // Connect reason code - 0 (Success)
            0x00, // Property length = 0
                  // No payload.
        ];

        let reader = PacketParser::new(&serialized_connack);
        let connack = ReceivedPacket::parse_message(&reader).unwrap();
        match connack {
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

        let reader = PacketParser::new(&serialized_publish);
        let publish = ReceivedPacket::parse_message(&reader).unwrap();
        match publish {
            ReceivedPacket::Publish(pub_info) => {
                assert_eq!(pub_info.topic, "A");
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_puback() {
        let serialized_suback: [u8; 6] = [
            0x40, // PubAck
            0x04, // Remaining length
            0x00, 0x05, // Identifier
            0x10, // Response Code
            0x00, // Properties length
        ];

        let reader = PacketParser::new(&serialized_suback);
        let puback = ReceivedPacket::parse_message(&reader).unwrap();
        match puback {
            ReceivedPacket::PubAck(pub_ack) => {
                assert_eq!(pub_ack.reason, 0x10);
                assert_eq!(pub_ack.packet_identifier, 5);
                assert_eq!(pub_ack.properties.len(), 0);
            }
            _ => panic!("Invalid message"),
        }
    }

    #[test]
    fn deserialize_good_puback_without_reason() {
        let serialized_suback: [u8; 4] = [
            0x40, // PubAck
            0x02, // Remaining length
            0x00, 0x06, // Identifier
        ];

        let reader = PacketParser::new(&serialized_suback);
        let puback = ReceivedPacket::parse_message(&reader).unwrap();
        match puback {
            ReceivedPacket::PubAck(pub_ack) => {
                assert_eq!(pub_ack.reason, 0x00);
                assert_eq!(pub_ack.packet_identifier, 6);
                assert_eq!(pub_ack.properties.len(), 0);
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

        let reader = PacketParser::new(&serialized_suback);
        let suback = ReceivedPacket::parse_message(&reader).unwrap();
        match suback {
            ReceivedPacket::SubAck(sub_ack) => {
                assert_eq!(sub_ack.reason_code, 2);
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

        let reader = PacketParser::new(&serialized_ping_req);
        let ping_req = ReceivedPacket::parse_message(&reader).unwrap();
        match ping_req {
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
        let reader = PacketParser::new(&serialized_pubcomp);
        let pub_comp = ReceivedPacket::parse_message(&reader).unwrap();
        match pub_comp {
            ReceivedPacket::PubComp(comp) => {
                assert_eq!(comp.packet_id, 5);
                assert_eq!(comp.reason_code, 0x16);
                assert_eq!(comp.properties.len(), 0);
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
        let reader = PacketParser::new(&serialized_pubcomp);
        let pub_comp = ReceivedPacket::parse_message(&reader).unwrap();
        match pub_comp {
            ReceivedPacket::PubComp(comp) => {
                assert_eq!(comp.packet_id, 5);
                assert_eq!(comp.reason_code, 0);
                assert_eq!(comp.properties.len(), 0);
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
        let reader = PacketParser::new(&serialized_pubrec);
        let pub_rec = ReceivedPacket::parse_message(&reader).unwrap();
        match pub_rec {
            ReceivedPacket::PubRec(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason_code, 0x10);
                assert_eq!(rec.properties.len(), 0);
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
        let reader = PacketParser::new(&serialized_pubrec);
        let pub_rec = ReceivedPacket::parse_message(&reader).unwrap();
        match pub_rec {
            ReceivedPacket::PubRec(rec) => {
                assert_eq!(rec.packet_id, 5);
                assert_eq!(rec.reason_code, 0);
                assert_eq!(rec.properties.len(), 0);
            }
            _ => panic!("Invalid message"),
        }
    }
}
