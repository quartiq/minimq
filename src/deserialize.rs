use crate::packet_reader::PacketReader;
use crate::minimq::{Error, MessageType, PubInfo, Meta};

use crate::properties::{
    Data,
    property_data,
    SUBSCRIPTION_IDENTIFIER,
    RESPONSE_TOPIC,
    CORRELATION_DATA
};

pub struct ConnAck {
    pub reason_code: u8,
}

pub struct SubAck {
    pub packet_identifier: u16,
    pub reason_code: u8,
}

pub enum ReceivedPacket {
    ConnAck(ConnAck),
    Publish(PubInfo),
    SubAck(SubAck),
}

pub fn parse_message<'a>(packet_reader: &'a mut PacketReader) -> Result<ReceivedPacket, Error> {
    let (message_type, _flags, remaining_length) = packet_reader.read_fixed_header()?;

    // Validate packet length.
    if remaining_length != packet_reader.len()? {
        return Err(Error::MalformedPacket);
    }

    // TODO: Validate flags

    match message_type {
        MessageType::ConnAck => {
            Ok(ReceivedPacket::ConnAck(parse_connack(packet_reader)?))
        },
        MessageType::Publish => {
            Ok(ReceivedPacket::Publish(parse_publish(packet_reader)?))
        },
        MessageType::SubAck => {
            Ok(ReceivedPacket::SubAck(parse_suback(packet_reader)?))
        },
        _ => Err(Error::UnsupportedPacket),
    }
}

fn parse_connack<'a>(p: &'a mut PacketReader) -> Result<ConnAck, Error> {
    let _ = p.read_u8()?; // ACK flags
    let reason_code = p.read_u8()?;

    Ok(ConnAck { reason_code })
}

fn parse_publish<'a>(p: &'a mut PacketReader) -> Result<PubInfo, Error> {
    let mut info = PubInfo::new();
    info.topic.len = p.read_utf8_string(&mut info.topic.buf)?;
    let properties_length = p.read_variable_length_integer()?;
    let payload_length = p.len()? - properties_length;
    while p.len()? > payload_length {
        match p.read_variable_length_integer()? {
            SUBSCRIPTION_IDENTIFIER => {
                info.sid = Some(p.read_variable_length_integer()?);
            }
            RESPONSE_TOPIC => {
                let mut response_topic = Meta::new(&[]);
                response_topic.len = p.read_utf8_string(&mut response_topic.buf)?;
                info.response = Some(response_topic);
            }
            CORRELATION_DATA => {
                let mut cd = Meta::new(&[]);
                cd.len = p.read_binary_data(&mut cd.buf)?;
                info.cd = Some(cd);
            }
            x => { skip_property(x, p)?; }
        }
    }

    if p.len()? != payload_length {
        return Err(Error::DataSize)
    }

    // Note that we intentionally don't read the payload from the data reader so that it is
    // available later to be borrowed directly to the handler for the payload data.
    Ok(info)
}

fn parse_suback<'a>(p: &'a mut PacketReader) -> Result<SubAck, Error> {
    // Read the variable length header.
    let id = p.read_u16()?;

    // Skip past all properties in the SubAck.
    let property_length = p.read_variable_length_integer()?;
    if property_length > p.len()? {
        return Err(Error::DataSize)
    }

    p.skip(property_length)?;

    // Read the final payload, which contains the reason code.
    let reason_code = p.read_u8()?;

    Ok(SubAck { packet_identifier: id, reason_code })
}

fn skip_property<'a>(property: usize, p: &'a mut PacketReader) -> Result<(), Error>{

    match property_data(property) {
        Some(Data::Byte) => {
            p.read_u8()?;
            Ok(())
        },
        Some(Data::TwoByteInteger) => {
            p.read_u16()?;
            Ok(())
        },
        Some(Data::FourByteInteger) => {
            p.read_u32()?;
            Ok(())
        },
        Some(Data::VariableByteInteger) => {
            p.read_variable_length_integer()?;
            Ok(())
        },
        Some(Data::BinaryData) | Some(Data::UTF8EncodedString) => {
            let len = p.read_variable_length_integer()?;
            p.skip(len)?;
            Ok(())
        },
        Some(Data::UTF8StringPair) => {
            let len = p.read_variable_length_integer()?;
            p.skip(len)?;

            let len = p.read_variable_length_integer()?;
            p.skip(len)?;

            Ok(())
        },
        None => Err(Error::UnknownProperty)
    }
}

#[test]
fn deserialize_good_connack() {
    let mut serialized_connack: [u8; 5] = [
        0x20,
        0x03, // Remaining length = 3 bytes
        0x00, // Connect acknowledge flags - bit 0 clear.
        0x00, // Connect reason code - 0 (Success)
        0x00, // Property length = 0
        // No payload.
    ];

    let mut reader = PacketReader::from_serialized(&mut serialized_connack);
    let connack = parse_message(&mut reader).unwrap();
    match connack {
        ReceivedPacket::ConnAck(conn_ack) => {
            assert_eq!(conn_ack.reason_code, 0);
        },
        _ => panic!("Invalid message"),
    }
}

#[test]
fn deserialize_good_publish() {
}

#[test]
fn deserialize_good_suback() {
}
