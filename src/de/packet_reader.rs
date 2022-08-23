use super::packet_parser::PacketParser;
use crate::ProtocolError as Error;

pub(crate) struct PacketReader<const T: usize> {
    pub buffer: [u8; T],
    read_bytes: usize,
    packet_length: Option<usize>,
}

impl<const T: usize> PacketReader<T> {
    pub fn new() -> PacketReader<T> {
        PacketReader {
            buffer: [0; T],
            read_bytes: 0,
            packet_length: None,
        }
    }

    pub fn receive_buffer(&mut self) -> Result<&mut [u8], Error> {
        if self.packet_length.is_none() {
            self.probe_fixed_header()?;
        }

        let end = if let Some(packet_length) = &self.packet_length {
            *packet_length
        } else {
            self.read_bytes + 1
        };

        Ok(&mut self.buffer[self.read_bytes..end])
    }

    pub fn commit(&mut self, count: usize) {
        self.read_bytes += count;
    }

    fn probe_fixed_header(&mut self) -> Result<(), Error> {
        if self.read_bytes <= 1 {
            return Ok(());
        }

        self.packet_length = None;

        let mut packet_length = 0;
        for (index, value) in self.buffer[1..self.read_bytes].iter().take(4).enumerate() {
            packet_length += ((value & 0x7F) as usize) << (index * 7);
            if (value & 0x80) == 0 {
                let length_size_bytes = 1 + index;

                // MQTT headers encode the packet type in the first byte followed by the packet
                // length as a varint
                let header_size_bytes = 1 + length_size_bytes;
                self.packet_length = Some(header_size_bytes + packet_length);
                break;
            }
        }

        // We should have found the packet length by now.
        if self.read_bytes >= 5 && self.packet_length.is_none() {
            return Err(Error::MalformedPacket);
        }

        Ok(())
    }

    pub fn packet_available(&self) -> bool {
        match self.packet_length {
            Some(length) => self.read_bytes >= length,
            None => false,
        }
    }

    pub fn reset(&mut self) {
        self.read_bytes = 0;
        self.packet_length = None;
    }

    pub fn received_packet(&self) -> Result<PacketParser<'_>, Error> {
        let packet_length = self.packet_length.as_ref().ok_or(Error::PacketSize)?;
        Ok(PacketParser::new(&self.buffer[..*packet_length]))
    }
}
