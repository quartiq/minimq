use crate::{
    de::PacketReader, message_types::MessageType, mqtt_client::ProtocolError as Error, Property,
};
use bit_field::BitField;
use generic_array::ArrayLength;
use heapless::{consts, Vec};

#[derive(Debug)]
pub struct ConnAck<'a> {
    /// Indicates true if session state is being maintained by the broker.
    pub session_present: bool,

    /// A status code indicating the success status of the connection.
    pub reason_code: u8,

    /// A list of properties associated with the connection.
    pub properties: Vec<Property<'a>, consts::U8>,
}

#[derive(Debug)]
pub struct Pub<'a> {
    /// The topic that the message was received on.
    pub topic: &'a str,

    /// The properties transmitted with the publish data.
    pub properties: Vec<Property<'a>, consts::U8>,
}

#[derive(Debug)]
pub struct SubAck<'a> {
    /// The identifier that the acknowledge is assocaited with.
    pub packet_identifier: u16,

    /// The success status of the subscription request.
    pub reason_code: u8,

    /// A list of properties associated with the subscription.
    pub properties: Vec<Property<'a>, consts::U8>,
}

#[derive(Debug)]
pub enum ReceivedPacket<'a> {
    ConnAck(ConnAck<'a>),
    Publish(Pub<'a>),
    SubAck(SubAck<'a>),
    PingResp,
}

impl<'a> ReceivedPacket<'a> {
    /// Parse a message out of a `PacketReader` into a validated MQTT control message.
    ///
    /// # Args
    /// * `packet_reader` - The reader to parse the message out of.
    ///
    /// # Returns
    /// A packet describing the received content.
    pub(crate) fn parse_message<'reader: 'a, T: ArrayLength<u8>>(
        packet_reader: &'reader PacketReader<T>,
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

            _ => Err(Error::UnsupportedPacket),
        }
    }
}

fn parse_connack<T: ArrayLength<u8>>(p: &PacketReader<T>) -> Result<ConnAck, Error> {
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

fn parse_publish<'a, 'reader: 'a, T: ArrayLength<u8>>(
    p: &'reader PacketReader<T>,
) -> Result<Pub<'a>, Error> {
    let topic = p.read_utf8_string()?;

    let properties = p.read_properties()?;
    // TODO: Validate properties associated with this message.

    // Note that we intentionally don't read the payload from the data reader so that it is
    // available later to be borrowed directly to the handler for the payload data.
    Ok(Pub { topic, properties })
}

fn parse_suback<T: ArrayLength<u8>>(p: &PacketReader<T>) -> Result<SubAck, Error> {
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

#[cfg(test)]
mod test {
    use super::{PacketReader, ReceivedPacket};
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
        let connack = ReceivedPacket::parse_message(&mut reader).unwrap();
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
        let publish = ReceivedPacket::parse_message(&mut reader).unwrap();
        match publish {
            ReceivedPacket::Publish(pub_info) => {
                assert_eq!(pub_info.topic, "A");
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
        let suback = ReceivedPacket::parse_message(&mut reader).unwrap();
        match suback {
            ReceivedPacket::SubAck(sub_ack) => {
                assert_eq!(sub_ack.reason_code, 2);
                assert_eq!(sub_ack.packet_identifier, 5);
            }
            _ => panic!("Invalid message"),
        }
    }
}
