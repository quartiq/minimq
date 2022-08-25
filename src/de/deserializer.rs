use serde::de::{DeserializeSeed, IntoDeserializer, Visitor};
use varint_rs::VarintReader;

#[derive(Debug)]
pub enum Error {
    WontImplement,
    Custom,
    BadString,
    BadBool,
    InsufficientData,
}

impl serde::de::Error for Error {
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
                Error::BadString => "Improper UTF-8 string encountered",
                Error::BadBool => "Bad boolean encountered",
                Error::InsufficientData => "Not enough data in the packet",
            }
        )
    }
}

pub struct MqttDeserializer<'a> {
    buf: &'a [u8],
    index: usize,
}

impl<'a> MqttDeserializer<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, index: 0 }
    }

    pub fn try_take_n(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.len() < n {
            return Err(Error::InsufficientData);
        }

        let data = &self.buf[self.index..self.index + n];
        self.index += n;
        Ok(data)
    }

    pub fn pop(&mut self) -> Result<u8, Error> {
        if self.len() == 0 {
            return Err(Error::InsufficientData);
        }

        let byte = self.buf[self.index];
        self.index += 1;
        Ok(byte)
    }

    pub fn read_u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes([self.pop()?, self.pop()?]))
    }

    pub fn len(&self) -> usize {
        self.buf.len() - self.index
    }

    pub fn read_varint(&mut self) -> Result<u32, Error> {
        self.read_u32_varint()
    }

    pub fn remainder(&self) -> &'a [u8] {
        &self.buf[self.index..]
    }
}

impl<'a> varint_rs::VarintReader for MqttDeserializer<'a> {
    type Error = Error;

    fn read(&mut self) -> Result<u8, Error> {
        self.pop()
    }
}

impl<'de, 'a> serde::de::Deserializer<'de> for &'a mut MqttDeserializer<'de> {
    type Error = Error;

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let val = match self.pop()? {
            0 => false,
            1 => true,
            _ => return Err(Error::BadBool),
        };
        visitor.visit_bool(val)
    }

    fn deserialize_i8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_i8(self.pop()? as i8)
    }

    fn deserialize_i16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_i16(i16::from_be_bytes([self.pop()?, self.pop()?]))
    }

    fn deserialize_i32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_i32(i32::from_be_bytes([
            self.pop()?,
            self.pop()?,
            self.pop()?,
            self.pop()?,
        ]))
    }

    fn deserialize_u8<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_u8(self.pop()?)
    }

    fn deserialize_u16<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_u16(self.read_u16()?)
    }

    fn deserialize_u32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor.visit_u32(u32::from_be_bytes([
            self.pop()?,
            self.pop()?,
            self.pop()?,
            self.pop()?,
        ]))
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let length = self.read_u16()?;
        let bytes: &'de [u8] = self.try_take_n(length as usize)?;
        let string = core::str::from_utf8(bytes).map_err(|_| Error::BadString)?;
        visitor.visit_borrowed_str(string)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_str(visitor)
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let length = self.read_u16()?;
        let bytes: &'de [u8] = self.try_take_n(length as usize)?;
        visitor.visit_borrowed_bytes(bytes)
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // Assume it is None there if there is remaining data.
        if self.len() == 0 {
            visitor.visit_none()
        } else {
            visitor.visit_some(self)
        }
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let len = self.read_varint()?;
        visitor.visit_seq(SeqAccess {
            deserializer: self,
            len: len as usize,
        })
    }

    fn deserialize_unit<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        // Nothing to decode.
        visitor.visit_unit()
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        crate::trace!("Deserialize tuple of {} bytes", len);
        visitor.visit_seq(ElementAccess {
            deserializer: self,
            count: len,
        })
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(len, visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(fields.len(), visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_enum(self)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_map<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        crate::trace!("Attempting to deserialize ID");
        Err(Error::WontImplement)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(
        self,
        _visitor: V,
    ) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }

    fn deserialize_char<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }

    fn deserialize_i64<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }

    fn deserialize_u64<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }
    fn deserialize_any<V: Visitor<'de>>(self, _visitor: V) -> Result<V::Value, Self::Error> {
        Err(Error::WontImplement)
    }
}

struct ElementAccess<'a, 'de: 'a> {
    deserializer: &'a mut MqttDeserializer<'de>,
    count: usize,
}

impl<'a, 'de: 'a> serde::de::SeqAccess<'de> for ElementAccess<'a, 'de> {
    type Error = Error;

    fn next_element_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<Option<V::Value>, Error> {
        if self.count > 0 {
            self.count -= 1;
            let data = DeserializeSeed::deserialize(seed, &mut *self.deserializer)?;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.count)
    }
}

struct SeqAccess<'a, 'de: 'a> {
    deserializer: &'a mut MqttDeserializer<'de>,
    len: usize,
}

impl<'a, 'de: 'a> serde::de::SeqAccess<'de> for SeqAccess<'a, 'de> {
    type Error = Error;

    fn next_element_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<Option<V::Value>, Error> {
        if self.len > 0 {
            let original_remaining = self.deserializer.len();
            let data = DeserializeSeed::deserialize(seed, &mut *self.deserializer)?;
            self.len -= original_remaining - self.deserializer.len();

            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    fn size_hint(&self) -> Option<usize> {
        None
    }
}

impl<'a, 'de> serde::de::VariantAccess<'de> for &'a mut MqttDeserializer<'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Error> {
        Ok(())
    }

    fn newtype_variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<V::Value, Error> {
        DeserializeSeed::deserialize(seed, self)
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        serde::de::Deserializer::deserialize_tuple(self, fields.len(), visitor)
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        serde::de::Deserializer::deserialize_tuple(self, len, visitor)
    }
}

impl<'a, 'de> serde::de::EnumAccess<'de> for &'a mut MqttDeserializer<'de> {
    type Error = Error;
    type Variant = Self;

    fn variant_seed<V: DeserializeSeed<'de>>(self, seed: V) -> Result<(V::Value, Self), Error> {
        let varint = self.read_varint()?;
        crate::trace!("Read Varint: 0x{:2X}", varint);
        let v = DeserializeSeed::deserialize(seed, varint.into_deserializer())?;
        Ok((v, self))
    }
}
