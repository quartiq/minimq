//! MQTT packet serializer.
//!
//! # Design
//! The serde implementation is intentionally narrow: packet structs describe MQTT field order, and
//! private wire helpers describe MQTT-specific field encodings such as UTF-8 strings, binary data,
//! properties, and varints. The encoder reserves fixed-header space, writes the variable header and
//! payload linearly, then backfills the MQTT fixed header.
//!
//! # Supported Data Types
//!
//! Basic data types are supported, including:
//! * Signed & Unsigned integers
//! * Booleans
//! * Strings
//! * Bytes
//! * Options (Some is encoded as the contained contents, None is not encoded as any data)
//! * Sequences
//! * Tuples
//! * Structs
//!
//! Other types are explicitly not implemented and there is no plan to implement them.
use crate::varint::VarintBuffer;
use crate::{
    packets::PublishHeader,
    wire::{ControlPacket, MessageType},
};
use serde::Serialize;

/// The maximum size of the MQTT fixed header in bytes. This accounts for the header byte and the
/// maximum variable integer length.
pub(crate) const MAX_FIXED_HEADER_SIZE: usize = 5;

/// Errors that result from the serialization process
#[derive(Debug, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub(crate) enum Error {
    /// The provided memory buffer did not have enough space to serialize into.
    InsufficientMemory,

    /// A custom serialization error occurred.
    Custom,
}

#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) enum PubError<E> {
    Encode(Error),
    Payload(E),
}

impl serde::ser::StdError for Error {}

impl serde::ser::Error for Error {
    fn custom<T: core::fmt::Display>(_msg: T) -> Self {
        crate::trace!("Serialization error");
        Error::Custom
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Error::Custom => "Custom serialization error",
                Error::InsufficientMemory => "Not enough data to encode the packet",
            }
        )
    }
}

/// Serializer for MQTT packet fields.
pub(crate) struct MqttSerializer<'a> {
    buf: &'a mut [u8],
    index: usize,
}

impl<'a> MqttSerializer<'a> {
    /// Construct a serializer over one packet buffer.
    pub(crate) fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            index: MAX_FIXED_HEADER_SIZE,
        }
    }

    /// Encode an MQTT control packet and return its start offset in the provided buffer.
    pub(crate) fn encode_with_offset<T: Serialize + ControlPacket>(
        buf: &'a mut [u8],
        packet: &T,
    ) -> Result<(usize, &'a [u8]), Error> {
        let mut serializer = Self::new(buf);
        packet.serialize(&mut serializer)?;
        let (offset, packet) = serializer.finalize(T::MESSAGE_TYPE, packet.fixed_header_flags())?;
        Ok((offset, packet))
    }

    pub(crate) fn encode_publish_with_offset<P: crate::publication::ToPayload>(
        buf: &'a mut [u8],
        header: &PublishHeader<'_>,
        payload: P,
    ) -> Result<(usize, &'a [u8]), PubError<P::Error>> {
        let mut serializer = Self::new(buf);
        header
            .serialize(&mut serializer)
            .map_err(PubError::Encode)?;

        let flags = header.fixed_header_flags();
        let len = payload
            .serialize(serializer.remainder())
            .map_err(PubError::Payload)?;
        serializer.commit(len).map_err(PubError::Encode)?;

        let (offset, packet) = serializer
            .finalize(MessageType::Publish, flags)
            .map_err(PubError::Encode)?;
        Ok((offset, packet))
    }

    pub(crate) fn encode_publish<P: crate::publication::ToPayload>(
        buf: &'a mut [u8],
        header: &PublishHeader<'_>,
        payload: P,
    ) -> Result<&'a [u8], PubError<P::Error>> {
        let (_, packet) = Self::encode_publish_with_offset(buf, header, payload)?;
        Ok(packet)
    }

    /// Encode an MQTT control packet into a buffer.
    pub(crate) fn encode<T: Serialize + ControlPacket>(
        buf: &'a mut [u8],
        packet: &T,
    ) -> Result<&'a [u8], Error> {
        let (_, packet) = Self::encode_with_offset(buf, packet)?;
        Ok(packet)
    }

    /// Finalize the packet, prepending the MQTT fixed header.
    ///
    /// # Args
    /// * `typ` - The MQTT message type of the encoded packet.
    /// * `flags` - The MQTT flags associated with the packet.
    ///
    /// # Returns
    /// A slice representing the serialized packet.
    pub(crate) fn finalize(self, typ: MessageType, flags: u8) -> Result<(usize, &'a [u8]), Error> {
        let len = self
            .index
            .checked_sub(MAX_FIXED_HEADER_SIZE)
            .ok_or(Error::InsufficientMemory)?;

        let mut buffer = VarintBuffer::new();
        crate::varint::write_mqtt_u32_varint(len as u32, &mut buffer)
            .map_err(|_| Error::InsufficientMemory)?;
        let remaining_len = buffer.as_slice();
        if self.buf.len() < MAX_FIXED_HEADER_SIZE {
            return Err(Error::InsufficientMemory);
        }

        // Write the remaining packet length.
        self.buf[MAX_FIXED_HEADER_SIZE - remaining_len.len()..MAX_FIXED_HEADER_SIZE]
            .copy_from_slice(remaining_len);

        // Write the header
        let header = ((typ as u8) << 4) | (flags & 0x0F);
        self.buf[MAX_FIXED_HEADER_SIZE - remaining_len.len() - 1] = header;

        let offset = MAX_FIXED_HEADER_SIZE - remaining_len.len() - 1;
        Ok((offset, &self.buf[offset..self.index]))
    }

    /// Write data into the packet.
    pub(crate) fn push_bytes(&mut self, data: &[u8]) -> Result<(), Error> {
        crate::trace!("Serializer pushed {=usize} bytes", data.len());
        if self.buf.len().saturating_sub(self.index) < data.len() {
            return Err(Error::InsufficientMemory);
        }

        self.buf[self.index..][..data.len()].copy_from_slice(data);
        self.index += data.len();

        Ok(())
    }

    /// Push a byte to the tail of the packet.
    pub(crate) fn push(&mut self, byte: u8) -> Result<(), Error> {
        if self.buf.len().saturating_sub(self.index) < 1 {
            return Err(Error::InsufficientMemory);
        }
        self.buf[self.index] = byte;
        self.index += 1;

        Ok(())
    }

    /// Get the remaining buffer to serialize into directly.
    ///
    /// # Note
    /// You must call `commit` after serializing.
    pub(crate) fn remainder(&mut self) -> &mut [u8] {
        let start = self.index.min(self.buf.len());
        &mut self.buf[start..]
    }

    /// Commit previously-serialized data into the buffer.
    pub(crate) fn commit(&mut self, len: usize) -> Result<(), Error> {
        if self.buf.len().saturating_sub(self.index) < len {
            return Err(Error::InsufficientMemory);
        }

        self.index += len;
        Ok(())
    }
}

impl serde::Serializer for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, v: bool) -> Result<Self::Ok, Self::Error> {
        self.push(v as u8)
    }

    fn serialize_i8(self, v: i8) -> Result<Self::Ok, Self::Error> {
        self.push(v as u8)
    }

    fn serialize_i16(self, v: i16) -> Result<Self::Ok, Self::Error> {
        self.push_bytes(&v.to_be_bytes())
    }

    fn serialize_i32(self, v: i32) -> Result<Self::Ok, Self::Error> {
        self.push_bytes(&v.to_be_bytes())
    }

    fn serialize_i64(self, v: i64) -> Result<Self::Ok, Self::Error> {
        self.push_bytes(&v.to_be_bytes())
    }

    fn serialize_u8(self, v: u8) -> Result<Self::Ok, Self::Error> {
        self.push(v)
    }

    fn serialize_u16(self, v: u16) -> Result<Self::Ok, Self::Error> {
        self.push_bytes(&v.to_be_bytes())
    }

    fn serialize_u32(self, v: u32) -> Result<Self::Ok, Self::Error> {
        self.push_bytes(&v.to_be_bytes())
    }

    fn serialize_u64(self, v: u64) -> Result<Self::Ok, Self::Error> {
        self.push_bytes(&v.to_be_bytes())
    }

    fn serialize_str(self, v: &str) -> Result<Self::Ok, Self::Error> {
        self.serialize_bytes(v.as_bytes())
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<Self::Ok, Self::Error> {
        self.push_bytes(v)
    }

    fn serialize_none(self) -> Result<(), Error> {
        Ok(())
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        Ok(())
    }

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Ok(self)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Error> {
        Ok(self)
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Error> {
        Ok(self)
    }

    fn serialize_char(self, _v: char) -> Result<Self::Ok, Self::Error> {
        Err(Error::Custom)
    }

    fn serialize_unit(self) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Err(Error::Custom)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(Error::Custom)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(Error::Custom)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(Error::Custom)
    }

    fn collect_str<T: ?Sized>(self, _value: &T) -> Result<Self::Ok, Error> {
        Err(Error::Custom)
    }

    fn serialize_f32(self, _v: f32) -> Result<Self::Ok, Self::Error> {
        Err(Error::Custom)
    }

    fn serialize_f64(self, _v: f64) -> Result<Self::Ok, Self::Error> {
        Err(Error::Custom)
    }
}

impl serde::ser::SerializeStruct for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}

impl serde::ser::SerializeSeq for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTuple for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}

impl serde::ser::SerializeTupleStruct for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, _value: &T) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::Custom)
    }
}

impl serde::ser::SerializeTupleVariant for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, _value: &T) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::Custom)
    }
}

impl serde::ser::SerializeMap for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, _key: &T) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, _value: &T) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::Custom)
    }
}

impl serde::ser::SerializeStructVariant for &mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        _value: &T,
    ) -> Result<(), Error> {
        Err(Error::Custom)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::Custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, MqttSerializer};
    use crate::{
        packets::{PingReq, PublishHeader},
        publication::Publication,
        wire::Utf8String,
    };

    #[test]
    fn control_packet_encode_rejects_buffers_smaller_than_fixed_header() {
        for len in 0..super::MAX_FIXED_HEADER_SIZE {
            let mut buf = vec![0u8; len];
            let result = MqttSerializer::encode(&mut buf, &PingReq);
            assert!(matches!(result, Err(Error::InsufficientMemory)));
        }
    }

    #[test]
    fn publish_encode_rejects_buffers_smaller_than_fixed_header() {
        for len in 0..super::MAX_FIXED_HEADER_SIZE {
            let mut buf = vec![0u8; len];
            let publication = Publication::bytes("a", b"x");
            let header = PublishHeader {
                topic: Utf8String(publication.topic),
                packet_id: None,
                properties: publication.properties,
                retain: publication.retain,
                qos: publication.qos,
                dup: false,
            };
            let result = MqttSerializer::encode_publish(&mut buf, &header, publication.payload);
            assert!(matches!(
                result,
                Err(crate::ser::PubError::Encode(Error::InsufficientMemory))
            ));
        }
    }
}
