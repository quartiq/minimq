use crate::{
    message_types::MessageType,
    mqtt_client::ProtocolError as Error,
    Property, {debug, warn},
};
use bit_field::BitField;
use generic_array::{ArrayLength, GenericArray};
use heapless::{consts, Vec};

// The maximum size of the fixed header. This is calculated as the type+flag byte and the maximum
// variable length integer size (4).
const FIXED_HEADER_MAX: usize = 5;

pub(crate) struct PacketReader<T: ArrayLength<u8>> {
    pub buffer: GenericArray<u8, T>,
    read_bytes: usize,
    packet_length: Option<usize>,
    index: core::cell::RefCell<usize>,
}

impl<T> PacketReader<T>
where
    T: ArrayLength<u8>,
{
    pub fn new() -> PacketReader<T> {
        PacketReader {
            buffer: GenericArray::default(),
            read_bytes: 0,
            packet_length: None,
            index: core::cell::RefCell::new(0),
        }
    }

    pub fn maximum_packet_length(&self) -> usize {
        self.buffer.len()
    }

    #[cfg(test)]
    pub fn from_serialized<'a>(buffer: &'a mut [u8]) -> PacketReader<T> {
        let len = buffer.len();
        let mut reader = PacketReader {
            buffer: GenericArray::default(),
            read_bytes: len,
            packet_length: None,
            index: core::cell::RefCell::new(0),
        };

        reader.buffer[..buffer.len()].copy_from_slice(&buffer);

        reader.probe_fixed_header();

        reader
    }

    pub fn payload(&self) -> Result<&[u8], Error> {
        Ok(&self.buffer[*self.index.borrow()..self.packet_length()?])
    }

    pub fn read(&self, dest: &mut [u8]) -> Result<(), Error> {
        let mut index = self.index.borrow_mut();

        if *index + dest.len() > self.packet_length()? {
            return Err(Error::DataSize);
        }

        dest.copy_from_slice(&self.buffer[*index..][..dest.len()]);
        *index += dest.len();

        Ok(())
    }

    fn read_borrowed(&self, count: usize) -> Result<&[u8], Error> {
        let mut index = self.index.borrow_mut();

        if *index + count > self.packet_length()? {
            return Err(Error::DataSize);
        }

        let borrowed_data = &self.buffer[*index..][..count];
        *index += count;

        Ok(borrowed_data)
    }

    pub fn len(&self) -> Result<usize, Error> {
        Ok(self.packet_length()? - *self.index.borrow())
    }

    pub fn read_variable_length_integer(&self) -> Result<usize, Error> {
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

        warn!("Encountered invalid variable integer");
        Err(Error::MalformedInteger)
    }

    pub fn read_fixed_header(&self) -> Result<(MessageType, u8, usize), Error> {
        let header = self.read_u8()?;
        let packet_length = self.read_variable_length_integer()?;

        Ok((
            MessageType::from(header.get_bits(4..=7)),
            header.get_bits(0..=3),
            packet_length,
        ))
    }

    pub fn read_utf8_string<'a, 'me: 'a>(&'me self) -> Result<&'a str, Error> {
        let string_length = self.read_u16()? as usize;

        if self.buffer.len() < string_length {
            return Err(Error::DataSize);
        }

        Ok(core::str::from_utf8(self.read_borrowed(string_length)?)
            .map_err(|_| Error::MalformedPacket)?)
    }

    pub fn read_binary_data(&self) -> Result<&[u8], Error> {
        let string_length = self.read_u16()? as usize;
        self.read_borrowed(string_length)
    }

    pub fn read_u32(&self) -> Result<u32, Error> {
        let mut buffer: [u8; 4] = [0; 4];
        self.read(&mut buffer)?;
        Ok(u32::from_be_bytes(buffer))
    }

    pub fn read_u16(&self) -> Result<u16, Error> {
        let mut buffer: [u8; 2] = [0; 2];
        self.read(&mut buffer)?;
        Ok(u16::from_be_bytes(buffer))
    }

    pub fn read_u8(&self) -> Result<u8, Error> {
        let mut byte: [u8; 1] = [0];
        self.read(&mut byte)?;

        Ok(byte[0])
    }

    pub fn read_properties<'a, 'me: 'a>(&'me self) -> Result<Vec<Property<'a>, consts::U8>, Error> {
        let mut properties: Vec<Property, consts::U8> = Vec::new();

        let properties_size = self.read_variable_length_integer()?;
        let mut property_bytes_processed = 0;

        while properties_size - property_bytes_processed > 0 {
            let property = Property::parse(self)?;
            property_bytes_processed += property.size();
            properties
                .push(property)
                .map_err(|_| Error::MalformedPacket)?;
        }

        if properties_size != property_bytes_processed {
            return Err(Error::MalformedPacket);
        }

        Ok(properties)
    }

    pub fn packet_length(&self) -> Result<usize, Error> {
        if let Some(packet_length) = self.packet_length {
            Ok(packet_length)
        } else {
            Err(Error::MalformedPacket)
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
        debug!("Popping packet of {} bytes", packet_length);

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
        self.index.replace(0);

        // Probe the fixed header to update the length in case a packet still exists to be
        // processed.
        self.probe_fixed_header();

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

        // Attempt to parse a variable byte integer out of the currently available data.
        self.packet_length = if let Some((rlen, nbytes)) = {
            let int = &self.buffer[1..self.read_bytes];

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
        } {
            Some(1 + rlen + nbytes)
        } else {
            None
        };

        self.packet_length
    }
}
