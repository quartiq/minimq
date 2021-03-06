use crate::{
    message_types::MessageType, mqtt_client::ProtocolError as Error,
    properties::PropertyIdentifier, ser::ReversedPacketWriter, Property,
};

pub fn integer_size(value: usize) -> usize {
    if value < 0x80 {
        1
    } else if value < 0x7FFF {
        2
    } else if value < 0x7F_FFFF {
        3
    } else if value < 0x7FFF_FFFF {
        4
    } else {
        panic!("Invalid integer");
    }
}

pub fn connect_message<'a, 'b>(
    dest: &'b mut [u8],
    client_id: &[u8],
    keep_alive: u16,
    properties: &[Property<'a>],
) -> Result<&'b [u8], Error> {
    // Validate the properties for this packet.
    for property in properties {
        match property.id() {
            PropertyIdentifier::SessionExpiryInterval
            | PropertyIdentifier::AuthenticationMethod
            | PropertyIdentifier::AuthenticationData
            | PropertyIdentifier::RequestProblemInformation
            | PropertyIdentifier::RequestResponseInformation
            | PropertyIdentifier::ReceiveMaximum
            | PropertyIdentifier::TopicAliasMaximum
            | PropertyIdentifier::UserProperty
            | PropertyIdentifier::MaximumPacketSize => {}

            _ => return Err(Error::InvalidProperty),
        }
    }

    let mut packet = ReversedPacketWriter::new(dest);

    // Write the payload, which is the client ID.
    packet.write_binary_data(client_id)?;

    // Write the variable header.
    packet.write_properties(properties)?;

    packet.write_u16(keep_alive)?;
    packet.write(&[0b10])?;
    packet.write(&[5])?;
    packet.write_utf8_string("MQTT")?;

    packet.finalize(MessageType::Connect, 0)
}

pub fn publish_message<'a, 'b, 'c>(
    dest: &'b mut [u8],
    topic: &'a str,
    payload: &[u8],
    properties: &[Property<'c>],
) -> Result<&'b [u8], Error> {
    // Validate the properties for this packet.
    for property in properties {
        match property.id() {
            PropertyIdentifier::ResponseTopic
            | PropertyIdentifier::PayloadFormatIndicator
            | PropertyIdentifier::MessageExpiryInterval
            | PropertyIdentifier::ContentType
            | PropertyIdentifier::CorrelationData
            | PropertyIdentifier::SubscriptionIdentifier
            | PropertyIdentifier::TopicAlias => {}
            _ => {
                return Err(Error::InvalidProperty);
            }
        };
    }

    let mut packet = ReversedPacketWriter::new(dest);

    // Write the payload into the packet.
    packet.write(payload)?;

    // Write the variable header into the packet.
    packet.write_properties(properties)?;

    // TODO: Handle packet ID when QoS > 0.
    packet.write_utf8_string(topic)?;

    // Write the fixed length header.
    packet.finalize(MessageType::Publish, 0)
}

pub fn subscribe_message<'a, 'b, 'c>(
    dest: &'c mut [u8],
    topic: &'b str,
    packet_id: u16,
    properties: &[Property<'a>],
) -> Result<&'c [u8], Error> {
    // Validate the properties for this packet.
    for property in properties {
        match property.id() {
            PropertyIdentifier::SubscriptionIdentifier => {}
            _ => {
                return Err(Error::InvalidProperty);
            }
        }
    }

    let mut packet = ReversedPacketWriter::new(dest);

    // TODO: Support multiple topics.
    // Write the payload (topic filter + options byte)
    packet.write(&[0])?;
    packet.write_utf8_string(topic)?;

    // Write the variable packet header.
    packet.write_properties(properties)?;
    packet.write_u16(packet_id)?;

    packet.finalize(MessageType::Subscribe, 0b0010)
}

pub fn ping_req_message<'a>(dest: &'a mut [u8]) -> Result<&'a [u8], Error> {
    ReversedPacketWriter::new(dest).finalize(MessageType::PingReq, 0b0000)
}

#[test]
pub fn serialize_publish() {
    let good_publish: [u8; 10] = [
        0x30, // Publish message
        0x08, // Remaining length (8)
        0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
        0x00, // Properties length
        0xAB, 0xCD, // Payload
    ];

    let mut buffer: [u8; 900] = [0; 900];
    let payload: [u8; 2] = [0xAB, 0xCD];
    let message = publish_message(&mut buffer, "ABC", &payload, &[]).unwrap();

    assert_eq!(message, good_publish);
}

#[test]
fn serialize_subscribe() {
    let good_subscribe: [u8; 11] = [
        0x82, // Subscribe request
        0x09, // Remaining length (11)
        0x00, 0x10, // Packet identifier (16)
        0x00, // Property length
        0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
        0x00, // Options byte = 0
    ];

    let mut buffer: [u8; 900] = [0; 900];
    let message = subscribe_message(&mut buffer, "ABC", 16, &[]).unwrap();

    assert_eq!(message, good_subscribe);
}

#[test]
pub fn serialize_publish_with_properties() {
    let good_publish: [u8; 14] = [
        0x30, // Publish message
        0x0c, // Remaining length (14)
        0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
        0x04, // Properties length - 1 property encoding a string of length 4
        0x08, 0x00, 0x01, 0x41, // Response topic "A"
        0xAB, 0xCD, // Payload
    ];

    let mut buffer: [u8; 900] = [0; 900];
    let payload: [u8; 2] = [0xAB, 0xCD];
    let message = publish_message(
        &mut buffer,
        "ABC",
        &payload,
        &[Property::ResponseTopic("A")],
    )
    .unwrap();

    assert_eq!(message, good_publish);
}

#[test]
fn serialize_connect() {
    let good_serialized_connect: [u8; 18] = [
        0x10, // Connect
        0x10, // Remaining length (16)
        0x00, 0x04, 0x4d, 0x51, 0x54, 0x54, 0x05, // MQTT5 header
        0x02, // Flags (Clean start)
        0x00, 0x0a, // Keep-alive (10)
        0x00, // Properties length
        0x00, 0x03, 0x41, 0x42, 0x43, // ABC client ID
    ];

    let mut buffer: [u8; 900] = [0; 900];
    let client_id = "ABC".as_bytes();
    let message = connect_message(&mut buffer, client_id, 10, &[]).unwrap();

    assert_eq!(message, good_serialized_connect)
}

#[test]
fn serialise_ping_req() {
    let good_ping_req: [u8; 2] = [0xc0, 0x00];
    let mut buffer: [u8; 900] = [0; 900];
    assert_eq!(ping_req_message(&mut buffer).unwrap(), good_ping_req)
}
