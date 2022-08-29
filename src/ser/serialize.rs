use super::serializer::MqttSerializer;
use crate::{packets::ControlPacket, ProtocolError as Error};
use serde::Serialize;

pub fn serialize_control_packet<'a, T: Serialize + ControlPacket + core::fmt::Debug>(
    buf: &'a mut [u8],
    packet: T,
) -> Result<&'a [u8], Error> {
    let mut serializer = MqttSerializer::new(buf);

    crate::debug!("Serializing {:?}: {:?}", T::MESSAGE_TYPE, packet);
    // TODO: Map this to a serialization error.
    packet
        .serialize(&mut serializer)
        .map_err(|_| Error::MalformedPacket)?;

    serializer
        .finalize(T::MESSAGE_TYPE, packet.fixed_header_flags())
        .map_err(|_| Error::MalformedPacket)
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

    let publish = crate::packets::Pub {
        qos: crate::QoS::AtMostOnce,
        packet_id: None,
        dup: false,
        properties: heapless::Vec::new(),
        retain: crate::Retain::NotRetained,
        topic: crate::types::Utf8String("ABC"),
        payload: &[0xAB, 0xCD],
    };

    let mut buffer: [u8; 900] = [0; 900];
    let message = serialize_control_packet(&mut buffer, publish).unwrap();

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

    let publish = crate::packets::Pub {
        qos: crate::QoS::AtLeastOnce,
        topic: crate::types::Utf8String("ABC"),
        packet_id: Some(0xBEEF),
        dup: false,
        properties: heapless::Vec::new(),
        retain: crate::Retain::NotRetained,
        payload: &[0xAB, 0xCD],
    };

    let mut buffer: [u8; 900] = [0; 900];
    let message = serialize_control_packet(&mut buffer, publish).unwrap();

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

    let subscribe = crate::packets::Subscribe {
        packet_id: 16,
        properties: crate::types::Properties(&[]),
        topics: &[(
            crate::types::Utf8String("ABC"),
            crate::types::SubscriptionOptions {},
        )],
    };

    let mut buffer: [u8; 900] = [0; 900];
    let message = serialize_control_packet(&mut buffer, subscribe).unwrap();

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

    let publish = crate::packets::Pub {
        qos: crate::QoS::AtMostOnce,
        topic: crate::types::Utf8String("ABC"),
        packet_id: None,
        dup: false,
        properties: heapless::Vec::from_slice(&[crate::properties::Property::ResponseTopic(
            crate::types::Utf8String("A"),
        )])
        .unwrap(),
        retain: crate::Retain::NotRetained,
        payload: &[0xAB, 0xCD],
    };

    let mut buffer: [u8; 900] = [0; 900];
    let message = serialize_control_packet(&mut buffer, publish).unwrap();

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
    let connect: crate::packets::Connect<'_, 1> = crate::packets::Connect {
        client_id: crate::types::Utf8String("ABC"),
        will: None,
        keep_alive: 10,
        properties: crate::types::Properties(&[]),
        clean_start: true,
    };

    let message = serialize_control_packet(&mut buffer, connect).unwrap();

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
    let mut will = crate::will::Will::<100>::new("EFG", &[0xAB, 0xCD], &[]).unwrap();
    will.qos(crate::QoS::AtMostOnce);
    will.retained(crate::Retain::NotRetained);

    let connect = crate::packets::Connect {
        clean_start: true,
        keep_alive: 10,
        properties: crate::types::Properties(&[]),
        client_id: crate::types::Utf8String("ABC"),
        will: Some(&will),
    };

    let message = serialize_control_packet(&mut buffer, connect).unwrap();

    assert_eq!(message, good_serialized_connect)
}

#[test]
fn serialize_ping_req() {
    let good_ping_req: [u8; 2] = [
        0xc0, // Ping
        0x00, // Remaining length (0)
    ];

    let mut buffer: [u8; 1024] = [0; 1024];
    let message = serialize_control_packet(&mut buffer, crate::packets::PingReq {}).unwrap();
    assert_eq!(message, good_ping_req);
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

    let pubrel = crate::packets::PubRel {
        packet_id: 5,
        code: 16,
        properties: crate::types::Properties(&[]),
    };

    let mut buffer: [u8; 1024] = [0; 1024];
    assert_eq!(
        serialize_control_packet(&mut buffer, pubrel).unwrap(),
        good_pubrel
    );
}
