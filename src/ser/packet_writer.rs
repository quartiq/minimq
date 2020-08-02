use crate::minimq::{Error, MessageType};

pub struct PacketWriter<'a> {
    buffer: &'a mut [u8],
    index: usize,
}

impl<'a> PacketWriter<'a> {
    pub fn new(data: &'a mut [u8]) -> Self {
        PacketWriter {
            buffer: data,
            index: 0,
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<(), Error> {
        if self.index + data.len() > self.buffer.len() {
            return Err(Error::Bounds);
        }

        self.buffer[self.index..][..data.len()].copy_from_slice(data);
        self.index += data.len();

        Ok(())
    }

    pub fn write_binary_data(&mut self, data: &[u8]) -> Result<(), Error> {
        if data.len() > u16::max as usize {
            return Err(Error::DataSize);
        }

        // Write the data length into the buffer.
        self.write(&(data.len() as u16).to_be_bytes())?;

        // Write the data into the buffer.
        self.write(data)
    }

    pub fn write_u16(&mut self, value: u16) -> Result<(), Error> {
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

    pub fn write_fixed_header(
        &mut self,
        typ: MessageType,
        flags: u8,
        len: usize,
    ) -> Result<(), Error> {
        // Write the control byte.
        let header: u8 = ((typ as u8) << 4) & 0xF0 | (flags & 0x0F);
        self.write(&[header])?;

        // Write the remaining packet length.
        self.write_variable_length_integer(len)
    }

    pub fn finalize(self) -> Result<usize, Error> {
        if self.index == 0 {
            Err(Error::EmptyPacket)
        } else {
            Ok(self.index)
        }
    }
}
