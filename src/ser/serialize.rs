use crate::minimq::{MessageType, PubInfo};
use crate::mqtt_client::ProtocolError as Error;
use crate::ser::PacketWriter;

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

pub fn connect_message(dest: &mut [u8], client_id: &[u8], keep_alive: u16) -> Result<usize, Error> {
    for i in 0..client_id.len() {
        if !(client_id[i] - 0x30 <=  9 || // 0-9
             client_id[i] - 0x41 <= 25 || // A-Z
             client_id[i] - 0x61 <= 25)
        {
            // a-z
            return Err(Error::Bounds);
        }
    }

    // Calculate the lengths
    let payload_length = client_id.len() + 2;
    let properties_length = 1;
    let variable_header_length = properties_length + 10;
    let packet_length = variable_header_length + payload_length;

    let mut packet = PacketWriter::new(dest);
    packet.write_fixed_header(MessageType::Connect, 0, packet_length)?;

    // Add the variable length header
    packet.write_utf8_string("MQTT")?;
    packet.write(&[5])?;
    packet.write(&[0b10])?;
    packet.write_u16(keep_alive)?;

    // Write length + properties (none)
    packet.write_variable_length_integer(0)?;

    // Write the payload, which is the client ID.
    packet.write_binary_data(client_id)?;

    packet.finalize()
}

pub fn publish_message(dest: &mut [u8], info: &PubInfo, payload: &[u8]) -> Result<usize, Error> {
    // Calculate the length of the packet and variable length header.
    let variable_header_length = info.variable_header_length();

    let mut packet = PacketWriter::new(dest);

    // Write the fixed length header.
    packet.write_fixed_header(
        MessageType::Publish,
        0,
        variable_header_length + payload.len(),
    )?;

    // Write the variable header into the packet.
    info.write_variable_header(&mut packet)?;

    // Write the payload into the packet.
    packet.write(payload)?;

    packet.finalize()
}

pub fn subscribe_message<'b>(
    dest: &mut [u8],
    topic: &'b str,
    packet_id: u16,
) -> Result<usize, Error> {
    let mut packet = PacketWriter::new(dest);

    let property_length = 0;
    let variable_header_length = property_length + integer_size(property_length) + 2;
    let payload_size = 3 + topic.len();
    let packet_length = variable_header_length + payload_size;

    packet.write_fixed_header(MessageType::Subscribe, 0b0010, packet_length)?;

    // Write the variable packet header.
    packet.write_u16(packet_id)?;

    // Write properties.
    packet.write_variable_length_integer(property_length)?;

    // Write the payload (topic)
    packet.write_utf8_string(topic)?;

    // Options byte
    packet.write(&[0])?;

    packet.finalize()
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

    let mut info = PubInfo::new();
    info.topic.set("ABC".as_bytes()).unwrap();

    let mut buffer: [u8; 900] = [0; 900];
    let payload: [u8; 2] = [0xAB, 0xCD];
    let length = publish_message(&mut buffer, &info, &payload).unwrap();

    assert_eq!(buffer[..length], good_publish);
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
    let length = subscribe_message(&mut buffer, "ABC", 16).unwrap();

    assert_eq!(buffer[..length], good_subscribe);
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
    let length = connect_message(&mut buffer, client_id, 10).unwrap();

    assert_eq!(buffer[..length], good_serialized_connect)
}
