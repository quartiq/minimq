//! Custom MQTT message serializer
//!
//! # Design
//! This serializer handles serializing MQTT packets.
//!
//! This serializer does _not_ assume MQTT packet format. It will encode data linearly into a
//! buffer and converts all data types to big-endian byte notation.
//!
//! The serializer reserves the first number of bytes for the MQTT fixed header, which is filled
//! out after the rest of the packet has been serialized.
//!
//! # Limitations
//! It is the responsibility of the user to handle prefixing necessary lengths and types on any
//! MQTT-specified datatypes, such as "Properties", "Binary Data", and "UTF-8 Encoded Strings".
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
use crate::message_types::MessageType;
use crate::varint::VarintBuffer;
use bit_field::BitField;
use serde::Serialize;
use varint_rs::VarintWriter;

/// The maximum size of the MQTT fixed header in bytes. This accounts for the header byte and the
/// maximum variable integer length.
const MAX_FIXED_HEADER_SIZE: usize = 5;

/// Errors that result from the serialization process
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Error {
    /// The provided memory buffer did not have enough space to serialize into.
    InsufficientMemory,

    /// The requested feature will not be implemented.
    WontImplement,

    /// A custom serialization error occurred.
    Custom,
}

impl serde::ser::Error for Error {
    fn custom<T: core::fmt::Display>(_msg: T) -> Self {
        crate::error!("{}", _msg);
        Error::Custom
    }
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(
            f,
            "{}",
            match self {
                Error::WontImplement => "This feature won't ever be implemented",
                Error::Custom => "Custom deserialization error",
                Error::InsufficientMemory => "Not enough data to encode the packet",
            }
        )
    }
}

/// A structure to serialize MQTT data into a buffer.
pub struct MqttSerializer<'a> {
    buf: &'a mut [u8],
    index: usize,
}

impl<'a> MqttSerializer<'a> {
    /// Construct a new serializer.
    ///
    /// # Args
    /// * `buf` - The location to serialize data into.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self {
            buf,
            index: MAX_FIXED_HEADER_SIZE,
        }
    }

    /// Finish the packet without prepending the MQTT fixed header.
    ///
    /// # Returns
    /// A slice representing the serialized packet without a fixed header.
    pub fn finish_without_fixed_header(self) -> &'a [u8] {
        &self.buf[MAX_FIXED_HEADER_SIZE..self.index]
    }

    /// Finalize the packet, prepending the MQTT fixed header.
    ///
    /// # Args
    /// * `typ` - The MQTT message type of the encoded packet.
    /// * `flags` - The MQTT flags associated with the packet.
    ///
    /// # Returns
    /// A slice representing the serialized packet.
    pub fn finalize(self, typ: MessageType, flags: u8) -> Result<&'a [u8], Error> {
        let len = self.index - MAX_FIXED_HEADER_SIZE;

        let mut buffer = VarintBuffer::new();
        buffer
            .write_u32_varint(len as u32)
            .map_err(|_| Error::InsufficientMemory)?;

        // Write the remaining packet length.
        self.buf[MAX_FIXED_HEADER_SIZE - buffer.data.len()..][..buffer.data.len()]
            .copy_from_slice(&buffer.data);

        // Write the header
        let header: u8 = *0u8.set_bits(4..8, typ as u8).set_bits(0..4, flags);
        self.buf[MAX_FIXED_HEADER_SIZE - buffer.data.len() - 1] = header;

        Ok(&self.buf[..self.index][MAX_FIXED_HEADER_SIZE - buffer.data.len() - 1..])
    }

    /// Write data into the packet.
    ///
    /// # Args
    /// * `data` - The data to push to the current head of the packet.
    pub fn push_bytes(&mut self, data: &[u8]) -> Result<(), Error> {
        crate::trace!("Pushing {:?}", data);
        if self.buf.len() - self.index < data.len() {
            return Err(Error::InsufficientMemory);
        }

        self.buf[self.index..][..data.len()].copy_from_slice(data);
        self.index += data.len();

        Ok(())
    }

    /// Push a byte to the tail of the packet.
    ///
    /// # Args
    /// * `byte` - The byte to write to the tail.
    pub fn push(&mut self, byte: u8) -> Result<(), Error> {
        if self.buf.len() - self.index < 1 {
            return Err(Error::InsufficientMemory);
        }
        self.buf[self.index] = byte;
        self.index += 1;

        Ok(())
    }
}

impl<'a> serde::Serializer for &mut MqttSerializer<'a> {
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
        Err(Error::WontImplement)
    }

    fn serialize_unit(self) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
    ) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Err(Error::WontImplement)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(Error::WontImplement)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(Error::WontImplement)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(Error::WontImplement)
    }

    fn collect_str<T: ?Sized>(self, _value: &T) -> Result<Self::Ok, Error> {
        Err(Error::WontImplement)
    }

    fn serialize_f32(self, _v: f32) -> Result<Self::Ok, Self::Error> {
        Err(Error::WontImplement)
    }

    fn serialize_f64(self, _v: f64) -> Result<Self::Ok, Self::Error> {
        Err(Error::WontImplement)
    }
}

impl<'a> serde::ser::SerializeStruct for &'a mut MqttSerializer<'_> {
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

impl<'a> serde::ser::SerializeSeq for &'a mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}

impl<'a> serde::ser::SerializeTuple for &'a mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut **self)
    }

    fn end(self) -> Result<(), Error> {
        Ok(())
    }
}

impl<'a> serde::ser::SerializeTupleStruct for &'a mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, _value: &T) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::WontImplement)
    }
}

impl<'a> serde::ser::SerializeTupleVariant for &'a mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, _value: &T) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::WontImplement)
    }
}

impl<'a> serde::ser::SerializeMap for &'a mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: ?Sized + Serialize>(&mut self, _key: &T) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn serialize_value<T: ?Sized + Serialize>(&mut self, _value: &T) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::WontImplement)
    }
}

impl<'a> serde::ser::SerializeStructVariant for &'a mut MqttSerializer<'_> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        _key: &'static str,
        _value: &T,
    ) -> Result<(), Error> {
        Err(Error::WontImplement)
    }

    fn end(self) -> Result<(), Error> {
        Err(Error::WontImplement)
    }
}
