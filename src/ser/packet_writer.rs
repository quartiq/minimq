/// Provides a means of serializing an MQTT control packet.
use crate::{MessageType, Property, ProtocolError as Error};

use bit_field::BitField;

/// Structure for writing a packet.
///
///
/// # Note
/// Serialization is performed tail -> head. That is, the last bytes of the packet are serialized
/// first. This is done so that packet section length calculation is trivial.
pub(crate) struct ReversedPacketWriter<'a> {
    buffer: &'a mut [u8],
    index: usize,
}

impl<'a> ReversedPacketWriter<'a> {
    /// Construct a new packet writer.
    ///
    /// # Args
    /// * `buffer` - The location to serialize the packet into.
    pub fn new(buffer: &'a mut [u8]) -> Self {
        let index = buffer.len();

        ReversedPacketWriter { buffer, index }
    }

    /// Write data at the tail of the packet.
    ///
    /// # Note
    /// The head of the packet is moved forward after this operation.
    ///
    /// # Args
    /// * `data` - The data to push to the current head of the packet.
    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.index < data.len() {
            return Err(Error::Bounds);
        }

        let write_start = self.index - data.len();

        self.buffer[write_start..self.index].copy_from_slice(data);
        self.index -= data.len();

        Ok(())
    }

    /// Write a binary block of data into the control packet.
    ///
    /// # Args
    /// * `data` - The binary data block to write.
    pub fn write_binary_data(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() > u16::max as usize {
            return Err(Error::DataSize);
        }

        // Write the data into the buffer.
        self.write(data)?;

        // Write the data length into the buffer.
        self.write(&(data.len() as u16).to_be_bytes())?;

        Ok(())
    }

    /// Write a 16-bit integer to the control packet.
    ///
    ///  # Args
    /// * `value` - The value to write.
    pub fn write_u16(&mut self, value: u16) -> Result<(), Error> {
        self.write(&value.to_be_bytes())
    }

    /// Write a 32-bit integer to the control packet.
    ///
    ///  # Args
    /// * `value` - The value to write.
    pub fn write_u32(&mut self, value: u32) -> Result<(), Error> {
        self.write(&value.to_be_bytes())
    }

    /// Write a UTF-8 encoded string into the control packet.
    ///
    ///  # Args
    /// * `string` - The string to encode.
    pub fn write_utf8_string<'b>(&mut self, string: &'b str) -> Result<(), Error> {
        self.write_binary_data(string.as_bytes())
    }

    /// Write a variable-length integer into the packet.
    ///
    ///  # Args
    /// * `value` - The integer to encode.
    pub fn write_variable_length_integer(&mut self, value: usize) -> Result<(), Error> {
        // The variable length integer can only support 28 bits.
        if value > 0x0FFF_FFFF {
            return Err(Error::DataSize);
        }

        if value & (0b0111_1111 << 21) > 0 {
            let data: [u8; 4] = [
                (value >> 21) as u8 & 0x7F,
                (value >> 14) as u8 | 0x80,
                (value >> 7) as u8 | 0x80,
                (value >> 0) as u8 | 0x80,
            ];

            self.write(&data)
        } else if value & (0b0111_1111 << 14) > 0 {
            let data: [u8; 3] = [
                (value >> 14) as u8 & 0x7F,
                (value >> 7) as u8 | 0x80,
                (value >> 0) as u8 | 0x80,
            ];

            self.write(&data)
        } else if value & (0b0111_1111 << 7) > 0 {
            let data: [u8; 2] = [(value >> 7) as u8 & 0x7F, (value >> 0) as u8 | 0x80];

            self.write(&data)
        } else {
            let data: [u8; 1] = [(value >> 0) as u8 & 0x7F];

            self.write(&data)
        }
    }

    /// Write the fixed header onto the packet.
    ///
    /// # Note
    /// This should be completed as the final step of packet serialization.
    ///
    /// # Args
    /// * `typ` - The control packet type.
    /// * `flags` - The flags associated with the control packet.
    /// * `
    fn write_fixed_header(&mut self, typ: MessageType, flags: u8, len: usize) -> Result<(), Error> {
        // Write the remaining packet length.
        self.write_variable_length_integer(len)?;

        // Write the control byte.
        let header: u8 = *0u8.set_bits(4..8, typ as u8).set_bits(0..4, flags);
        self.write(&[header])?;

        Ok(())
    }

    pub fn current_length(&self) -> usize {
        self.buffer.len() - self.index
    }

    pub fn write_properties<'b>(&mut self, properties: &[Property<'b>]) -> Result<(), Error> {
        let start_length = self.current_length();

        for property in properties {
            property.encode_into(self)?;
        }

        // Store the length of all the properties.
        self.write_variable_length_integer(self.current_length() - start_length)?;

        Ok(())
    }

    pub fn finalize(mut self, typ: MessageType, flags: u8) -> Result<&'a [u8], Error> {
        // Write the fixed header.
        self.write_fixed_header(typ, flags, self.current_length())?;

        if self.index == self.buffer.len() {
            Err(Error::MalformedPacket)
        } else {
            Ok(&self.buffer[self.index..])
        }
    }
}
