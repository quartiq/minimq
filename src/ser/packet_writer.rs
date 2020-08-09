use crate::minimq::MessageType;
use crate::mqtt_client::ProtocolError as Error;
use crate::Property;

pub(crate) struct ReversedPacketWriter<'a> {
    buffer: &'a mut [u8],
    index: usize,
}

impl<'a> ReversedPacketWriter<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        let index = buffer.len();

        ReversedPacketWriter { buffer, index }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.index < data.len() {
            return Err(Error::Bounds);
        }

        let write_start = self.index - data.len();

        self.buffer[write_start..self.index].copy_from_slice(data);
        self.index -= data.len();

        Ok(())
    }

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

    pub fn write_u16(&mut self, value: u16) -> Result<(), Error> {
        self.write(&value.to_be_bytes())
    }

    pub fn write_u32(&mut self, value: u32) -> Result<(), Error> {
        self.write(&value.to_be_bytes())
    }

    pub fn write_utf8_string<'b>(&mut self, string: &'b str) -> Result<(), Error> {
        self.write_binary_data(string.as_bytes())
    }

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

    fn write_fixed_header(&mut self, typ: MessageType, flags: u8, len: usize) -> Result<(), Error> {
        // Write the remaining packet length.
        self.write_variable_length_integer(len)?;

        // Write the control byte.
        let header: u8 = ((typ as u8) << 4) & 0xF0 | (flags & 0x0F);
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
