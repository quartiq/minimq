use crate::{
    message_types::MessageType, properties::PropertyIdentifier, ser::ReversedPacketWriter,
    will::Will, Property, ProtocolError as Error, QoS, Retain,
};

use bit_field::BitField;

pub fn connect_packet<'a, const S: usize>(
    dest: &'a mut [u8],
    client_id: &[u8],
    keep_alive: u16,
    properties: &[Property],
    clean_start: bool,
    will: Option<&Will<S>>,
) -> Result<&'a [u8], Error> {
    // Validate the properties for this packet.
    for property in properties {
        match property.into() {
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

    let mut flags: u8 = 0;
    flags.set_bit(1, clean_start);

    let mut packet = ReversedPacketWriter::new(dest);

    if let Some(will) = will {
        // Serialize the will data into the packet
        packet.write(&will.payload)?;

        // Update the flags for the will parameters. Indicate that the will is present, the QoS of
        // the will message, and whether or not the will message should be retained.
        flags.set_bit(2, true);
        flags.set_bits(3..=4, will.qos as u8);
        flags.set_bit(5, will.retain == Retain::Retained);
    }

    // Write the the client ID payload.
    packet.write_binary_data(client_id)?;

    // Write the variable header.
    packet.write_properties(properties)?;

    packet.write_u16(keep_alive)?;
    packet.write(&[flags])?;
    packet.write(&[5])?;
    packet.write_utf8_string("MQTT")?;

    packet.finalize(MessageType::Connect, 0)
}

pub fn ping_req_packet(dest: &mut [u8]) -> Result<&[u8], Error> {
    ReversedPacketWriter::new(dest).finalize(MessageType::PingReq, 0x00)
}

pub fn publish_packet<'a, 'b, 'c>(
    dest: &'b mut [u8],
    topic: &'a str,
    payload: &[u8],
    qos: QoS,
    retain: Retain,
    id: u16,
    properties: &[Property<'c>],
) -> Result<&'b [u8], Error> {
    // Validate the properties for this packet.
    for property in properties {
        match property.into() {
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

    if qos != QoS::AtMostOnce {
        packet.write_u16(id)?;
    }

    packet.write_utf8_string(topic)?;

    // Set qos to fixed header bits 1 and 2 in binary
    let flags = *0u8
        .set_bits(1..=2, qos as u8)
        .set_bit(0, retain == Retain::Retained);

    // Write the fixed length header.
    packet.finalize(MessageType::Publish, flags)
}

pub fn subscribe_packet<'a, 'b, 'c>(
    dest: &'c mut [u8],
    topic: &'b str,
    packet_id: u16,
    properties: &[Property<'a>],
) -> Result<&'c [u8], Error> {
    // Validate the properties for this packet.
    for property in properties {
        match property.into() {
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

pub fn pubrel_packet<'c, 'a>(
    dest: &'c mut [u8],
    packet_id: u16,
    reason: u8,
    properties: &[Property<'a>],
) -> Result<&'c [u8], Error> {
    let mut packet = ReversedPacketWriter::new(dest);

    // Properties and reason can be omitted if there reason code is zero and there are no
    // properties.
    if !(properties.is_empty() && reason == 0) {
        packet.write_properties(properties)?;
        packet.write(&[reason])?;
    }

    packet.write_u16(packet_id)?;
    packet.finalize(MessageType::PubRel, 0b0010)
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
    let message = publish_packet(
        &mut buffer,
        "ABC",
        &payload,
        QoS::AtMostOnce,
        Retain::NotRetained,
        0,
        &[],
    )
    .unwrap();

    assert_eq!(message, good_publish);
}

#[test]
pub fn serialize_publish_qos1() {
    let good_publish: [u8; 12] = [
        0x32, // Publish message
        0x0a, // Remaining length (10)
        0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
        0xBE, 0xEF, // Packet identifier
        0x00, // Properties length
        0xAB, 0xCD, // Payload
    ];

    let mut buffer: [u8; 900] = [0; 900];
    let payload: [u8; 2] = [0xAB, 0xCD];
    let message = publish_packet(
        &mut buffer,
        "ABC",
        &payload,
        QoS::AtLeastOnce,
        Retain::NotRetained,
        0xbeef,
        &[],
    )
    .unwrap();

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
    let message = subscribe_packet(&mut buffer, "ABC", 16, &[]).unwrap();

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
    let message = publish_packet(
        &mut buffer,
        "ABC",
        &payload,
        QoS::AtMostOnce,
        Retain::NotRetained,
        0,
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
    let message = connect_packet::<100>(&mut buffer, client_id, 10, &[], true, None).unwrap();

    assert_eq!(message, good_serialized_connect)
}

#[test]
fn serialize_connect_with_will() {
    #[rustfmt::skip]
    let good_serialized_connect: [u8; 28] = [
        0x10, // Connect
        26, // Remaining length

        // Header: "MQTT5"
        0x00, 0x04, 0x4d, 0x51, 0x54, 0x54, 0x05,

        // Flags: Clean start, will present, will not retained, will QoS = 0,
        0b0000_0110,

        // Keep-alive: 10 seconds
        0x00, 0x0a,

        // Connected Properties: None
        0x00,
        // Client ID: "ABC"
        0x00, 0x03, 0x41, 0x42, 0x43,
        // Will properties: None
        0x00,
        // Will topic: "EFG"
        0x00, 0x03, 0x45, 0x46, 0x47,
        // Will payload: [0xAB, 0xCD]
        0x00, 0x02, 0xAB, 0xCD,
    ];

    let mut buffer: [u8; 900] = [0; 900];
    let client_id = "ABC".as_bytes();
    let mut will = Will::<100>::new("EFG", &[0xAB, 0xCD], &[]).unwrap();
    will.qos(QoS::AtMostOnce);
    will.retained(Retain::NotRetained);

    let message = connect_packet(&mut buffer, client_id, 10, &[], true, Some(&will)).unwrap();

    assert_eq!(message, good_serialized_connect)
}

#[test]
fn serialize_ping_req() {
    let good_ping_req: [u8; 2] = [
        0xc0, // Ping
        0x00, // Remaining length (0)
    ];

    let mut buffer: [u8; 1024] = [0; 1024];
    assert_eq!(ping_req_packet(&mut buffer).unwrap(), good_ping_req);
}

#[test]
fn serialize_pubrel() {
    let good_pubrel: [u8; 6] = [
        6 << 4 | 0b10, // PubRec
        0x04,          // Remaining length
        0x00,
        0x05, // Identifier
        0x10, // Response Code
        0x00, // Properties length
    ];

    let mut buffer: [u8; 1024] = [0; 1024];
    assert_eq!(
        pubrel_packet(&mut buffer, 5, 0x10, &[]).unwrap(),
        good_pubrel
    );
}

#[test]
fn serialize_short_pubrel() {
    let good_pubrel: [u8; 4] = [
        6 << 4 | 0b10, // PubRec
        0x02,          // Remaining length
        0x00,
        0x05, // Identifier
    ];

    let mut buffer: [u8; 1024] = [0; 1024];
    assert_eq!(
        pubrel_packet(&mut buffer, 5, 0x0, &[]).unwrap(),
        good_pubrel
    );
}
