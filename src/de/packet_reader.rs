use crate::minimq::{Error, MessageType};
use bit_field::BitField;

const FIXED_HEADER_MAX: usize = 5; // type/flags + remaining length

// TODO: Refactor to remove this.
fn parse_variable_byte_integer(int: &[u8]) -> Option<(usize, usize)> {
    let len = if int.len() >= 1 && (int[0] & 0b1000_0000) == 0 {
        1
    } else if int.len() >= 2 && (int[1] & 0b1000_0000) == 0 {
        2
    } else if int.len() >= 3 && (int[2] & 0b1000_0000) == 0 {
        3
    } else if int.len() >= 4 && (int[3] & 0b1000_0000) == 0 {
        4
    } else {
        return None;
    };
    let mut acc = 0;
    for i in 0..len {
        acc += ((int[i] & 0b0111_1111) as usize) << i * 7;
    }
    Some((acc, len))
}

pub struct PacketReader<'a> {
    pub buffer: &'a mut [u8],
    read_bytes: usize,
    packet_length: Option<usize>,
    index: usize,
}

impl<'a> PacketReader<'a> {
    pub fn new(buffer: &'a mut [u8]) -> PacketReader {
        PacketReader {
            buffer: buffer,
            read_bytes: 0,
            packet_length: None,
            index: 0,
        }
    }

    pub fn from_serialized(buffer: &'a mut [u8]) -> PacketReader {
        let len = buffer.len();
        let mut reader = PacketReader {
            buffer: buffer,
            read_bytes: len,
            packet_length: None,
            index: 0,
        };

        reader.probe_fixed_header();

        reader
    }

    pub fn payload(&self) -> Result<&[u8], Error> {
        Ok(&self.buffer[self.index..self.packet_length()?])
    }

    pub fn read(&mut self, dest: &mut [u8]) -> Result<(), Error> {
        if self.index + dest.len() > self.packet_length()? {
            return Err(Error::DataSize);
        }

        dest.copy_from_slice(&self.buffer[self.index..][..dest.len()]);
        self.index += dest.len();

        Ok(())
    }

    pub fn skip(&mut self, bytes: usize) -> Result<(), Error> {
        if self.index + bytes > self.buffer.len() {
            return Err(Error::DataSize);
        }

        self.index += bytes;

        Ok(())
    }

    pub fn len(&self) -> Result<usize, Error> {
        Ok(self.packet_length()? - self.index)
    }

    pub fn read_variable_length_integer(&mut self) -> Result<usize, Error> {
        let mut bytes: [u8; 4] = [0; 4];

        let mut accumulator: usize = 0;
        let mut multiplier = 1;
        for i in 0..bytes.len() {
            self.read(&mut bytes[i..i + 1])?;
            accumulator += bytes[i] as usize & 0x7F * multiplier;
            multiplier *= 128;

            if (bytes[i] & 0x80) == 0 {
                return Ok(accumulator);
            }
        }

        Err(Error::MalformedInteger)
    }

    pub fn read_fixed_header(&mut self) -> Result<(MessageType, u8, usize), Error> {
        let header = self.read_u8()?;
        let packet_length = self.read_variable_length_integer()?;

        Ok((
            MessageType::from(header.get_bits(4..=7)),
            header.get_bits(0..=3),
            packet_length,
        ))
    }

    pub fn read_utf8_string(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        let string_length = self.read_u16()? as usize;

        if buf.len() < string_length {
            return Err(Error::DataSize);
        }

        self.read(&mut buf[..string_length])?;

        Ok(string_length)
    }

    pub fn read_binary_data(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        self.read_utf8_string(buf)
    }

    pub fn read_u32(&mut self) -> Result<u32, Error> {
        let mut bytes: [u8; 4] = [0; 4];
        self.read(&mut bytes)?;

        Ok(u32::from_be_bytes(bytes))
    }

    pub fn read_u16(&mut self) -> Result<u16, Error> {
        let mut bytes: [u8; 2] = [0; 2];
        self.read(&mut bytes)?;

        Ok(u16::from_be_bytes(bytes))
    }

    pub fn read_u8(&mut self) -> Result<u8, Error> {
        let mut byte: [u8; 1] = [0];
        self.read(&mut byte)?;

        Ok(byte[0])
    }

    pub fn message_type(&self) -> MessageType {
        assert!(self.buffer.len() > 0);
        MessageType::from(self.buffer[0].get_bits(4..7))
    }

    pub fn packet_length(&self) -> Result<usize, Error> {
        if let Some(packet_length) = self.packet_length {
            Ok(packet_length)
        } else {
            Err(Error::EmptyPacket)
        }
    }

    pub fn packet_available(&self) -> bool {
        match self.packet_length {
            Some(length) => self.read_bytes >= length,
            None => false,
        }
    }

    pub fn pop_packet(&mut self) -> Result<(), Error> {
        let packet_length = self.packet_length()?;

        let move_length = self.read_bytes - packet_length;

        // Move data after the packet to the front.
        unsafe {
            let head: *mut u8 = &mut self.buffer[0] as *mut u8;
            let tail: *const u8 = &self.buffer[packet_length] as *const u8;
            core::intrinsics::copy(tail, head, move_length);
        };

        // Reset the read_bytes counter.
        self.read_bytes = move_length;

        // Reset the reader index.
        self.index = 0;

        Ok(())
    }

    pub fn reset(&mut self) {
        self.read_bytes = 0;
        self.packet_length = None;
    }

    pub fn fill(&mut self, stream: &[u8]) -> usize {
        let read = core::cmp::min(stream.len(), self.buffer.len() - self.read_bytes);
        self.buffer[self.read_bytes..][..read].copy_from_slice(&stream[..read]);
        self.read_bytes += read;
        read
    }

    pub fn slurp(&mut self, stream: &[u8]) -> Result<usize, Error> {
        let read = self.fill(stream);
        if let Some(total_len) = self.probe_fixed_header() {
            if self.packet_length != None {
                if total_len > self.buffer.len() {
                    return Err(Error::PacketSize);
                }
            } else if self.read_bytes >= FIXED_HEADER_MAX {
                return Err(Error::MalformedPacket);
            }
        }

        Ok(read)
    }

    pub fn probe_fixed_header(&mut self) -> Option<usize> {
        if self.read_bytes <= 1 {
            return None;
        }

        self.packet_length = match parse_variable_byte_integer(&self.buffer[1..self.read_bytes]) {
            Some((rlen, nbytes)) => Some(1 + nbytes + rlen),
            None => None,
        };

        self.packet_length
    }
}
