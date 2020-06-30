use crate::packet_writer::PacketWriter;
use crate::minimq::{Error, MessageType, PubInfo};
use crate::properties::SUBSCRIPTION_IDENTIFIER;

fn integer_size(value: usize) -> usize {
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
             client_id[i] - 0x61 <= 25) { // a-z
            return Err(Error::Bounds);
        }
    }

    // Calculate the lengths
    let payload_length = client_id.len() + 2;
    let properties_length = 2;
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
    packet.write_fixed_header(MessageType::Publish, 0, variable_header_length + payload.len())?;

    // Write the variable header into the packet.
    info.write_variable_header(&mut packet)?;

    // Write the payload into the packet.
    packet.write(payload)?;

    packet.finalize()
}

pub fn subscribe_message<'b>(dest: &mut [u8], topic: &'b str, sender_id: usize, packet_id: u16) -> Result<usize, Error>
{
    if !(sender_id > 0 && sender_id <= 0x0F_FF_FF_FF) {
        return Err(Error::Bounds);
    }

    let mut packet = PacketWriter::new(dest);

    let property_length = integer_size(sender_id) + 2;
    let variable_header_length = property_length + 2;
    let payload_size = 3 + topic.len();
    let packet_length = variable_header_length + payload_size;

    packet.write_fixed_header(MessageType::Subscribe, 0b0010, packet_length)?;

    // Write the variable packet header.
    packet.write_u16(packet_id)?;

    // Write properties.
    packet.write_u16(property_length as u16)?;
    packet.write_u16(SUBSCRIPTION_IDENTIFIER as u16)?;
    packet.write_variable_length_integer(sender_id)?;

    // Write the payload (topic)
    packet.write_utf8_string(topic)?;

    // Options byte
    packet.write(&[0])?;

    packet.finalize()
}
