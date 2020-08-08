use crate::de::PacketReader;
use crate::minimq::{MessageType, Meta, PubInfo};
use crate::mqtt_client::ProtocolError as Error;
use generic_array::ArrayLength;

use bit_field::BitField;

use crate::properties::{
    property_data, Data, CORRELATION_DATA, RESPONSE_TOPIC, SUBSCRIPTION_IDENTIFIER,
};

pub struct ConnAck {
    pub session_present: bool,
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

pub fn parse_message<T: ArrayLength<u8>>(
    packet_reader: &mut PacketReader<T>,
) -> Result<ReceivedPacket, Error> {
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

        MessageType::SubAck => {
            if flags != 0 {
                return Err(Error::MalformedPacket);
            }

            Ok(ReceivedPacket::SubAck(parse_suback(packet_reader)?))
        }

        _ => Err(Error::UnsupportedPacket),
    }
}

fn parse_connack<T: ArrayLength<u8>>(p: &mut PacketReader<T>) -> Result<ConnAck, Error> {
    // Read the connect acknowledgement flags.
    let flags = p.read_u8()?;
    if flags != 0 && flags != 1 {
        return Err(Error::MalformedPacket);
    }

    let reason_code = p.read_u8()?;

    // TODO: Parse properties.

    Ok(ConnAck {
        reason_code,
        session_present: flags.get_bit(0),
    })
}

fn parse_publish<T: ArrayLength<u8>>(p: &mut PacketReader<T>) -> Result<PubInfo, Error> {
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
            x => {
                skip_property(x, p)?;
            }
        }
    }

    if p.len()? != payload_length {
        return Err(Error::DataSize);
    }

    // Note that we intentionally don't read the payload from the data reader so that it is
    // available later to be borrowed directly to the handler for the payload data.
    Ok(info)
}

fn parse_suback<T: ArrayLength<u8>>(p: &mut PacketReader<T>) -> Result<SubAck, Error> {
    // Read the variable length header.
    let id = p.read_u16()?;

    // Skip past all properties in the SubAck.
    let property_length = p.read_variable_length_integer()?;
    if property_length > p.len()? {
        return Err(Error::DataSize);
    }

    p.skip(property_length)?;

    // Read the final payload, which contains the reason code.
    let reason_code = p.read_u8()?;

    Ok(SubAck {
        packet_identifier: id,
        reason_code,
    })
}

fn skip_property<T: ArrayLength<u8>>(
    property: usize,
    p: &mut PacketReader<T>,
) -> Result<(), Error> {
    match property_data(property) {
        Some(Data::Byte) => {
            p.read_u8()?;
            Ok(())
        }
        Some(Data::TwoByteInteger) => {
            p.read_u16()?;
            Ok(())
        }
        Some(Data::FourByteInteger) => {
            p.read_u32()?;
            Ok(())
        }
        Some(Data::VariableByteInteger) => {
            p.read_variable_length_integer()?;
            Ok(())
        }
        Some(Data::BinaryData) | Some(Data::UTF8EncodedString) => {
            let len = p.read_variable_length_integer()?;
            p.skip(len)?;
            Ok(())
        }
        Some(Data::UTF8StringPair) => {
            let len = p.read_variable_length_integer()?;
            p.skip(len)?;

            let len = p.read_variable_length_integer()?;
            p.skip(len)?;

            Ok(())
        }
        None => Err(Error::UnknownProperty),
    }
}

#[cfg(test)]
use generic_array::typenum;

#[test]
fn deserialize_good_connack() {
    let mut serialized_connack: [u8; 5] = [
        0x20, 0x03, // Remaining length = 3 bytes
        0x00, // Connect acknowledge flags - bit 0 clear.
        0x00, // Connect reason code - 0 (Success)
        0x00, // Property length = 0
              // No payload.
    ];

    let mut reader = PacketReader::<typenum::U32>::from_serialized(&mut serialized_connack);
    let connack = parse_message(&mut reader).unwrap();
    match connack {
        ReceivedPacket::ConnAck(conn_ack) => {
            assert_eq!(conn_ack.reason_code, 0);
        }
        _ => panic!("Invalid message"),
    }
}

#[test]
fn deserialize_good_publish() {
    let mut serialized_publish: [u8; 7] = [
        0x30, // Publish, no QoS
        0x04, // Remaining length
        0x00, 0x01, // Topic length (1)
        0x41, // Topic name: 'A'
        0x00, // Properties length
        0x05, // Payload
    ];

    let mut reader = PacketReader::<typenum::U32>::from_serialized(&mut serialized_publish);
    let publish = parse_message(&mut reader).unwrap();
    match publish {
        ReceivedPacket::Publish(pub_info) => {
            assert_eq!(pub_info.topic.get(), "A".as_bytes());
        }
        _ => panic!("Invalid message"),
    }
}

#[test]
fn deserialize_good_suback() {
    let mut serialized_suback: [u8; 6] = [
        0x90, // SubAck
        0x04, // Remaining length
        0x00, 0x05, // Identifier
        0x00, // Properties length
        0x02, // Response Code
    ];

    let mut reader = PacketReader::<typenum::U32>::from_serialized(&mut serialized_suback);
    let suback = parse_message(&mut reader).unwrap();
    match suback {
        ReceivedPacket::SubAck(sub_ack) => {
            assert_eq!(sub_ack.reason_code, 2);
            assert_eq!(sub_ack.packet_identifier, 5);
        }
        _ => panic!("Invalid message"),
    }
}
