//! MQTT-Specific Data Types
//!
//! This module provides wrapper methods and serde functionality for MQTT-specified data types.
use crate::{properties::Property, varint::Varint};
use serde::ser::SerializeStruct;

/// A wrapper type for a number of MQTT `Property`s.
///
/// # Note
/// This wrapper type is primarily used to support custom serialization.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Properties<'a>(pub &'a [Property<'a>]);

impl<'a> serde::Serialize for Properties<'a> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut item = serializer.serialize_struct("Properties", 0)?;

        // Properties in MQTTv5 must be prefixed with a variable-length integer denoting the size
        // of the all of the properties in bytes.
        let property_length = self.0.iter().fold(0, |len, prop| len + prop.size());
        item.serialize_field("_len", &Varint(property_length as u32))?;
        item.serialize_field("_props", self.0)?;
        item.end()
    }
}

/// A wrapper type for "Binary Data" as defined in the MQTT v5 specification.
///
/// # Note
/// This wrapper type is primarily used to support custom serde functionality.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BinaryData<'a>(pub &'a [u8]);

impl<'a> serde::Serialize for BinaryData<'a> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        if self.0.len() > u16::MAX as usize {
            return Err(S::Error::custom("Provided string is too long"));
        }

        let len = self.0.len() as u16;
        let mut item = serializer.serialize_struct("_BinaryData", 0)?;

        // Binary data in MQTTv5 must be transmitted with a prefix of its length in bytes as a u16.
        item.serialize_field("_len", &len)?;
        item.serialize_field("_data", self.0)?;
        item.end()
    }
}

impl<'de> serde::de::Deserialize<'de> for BinaryData<'de> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct BinaryDataVisitor;

        impl<'de> serde::de::Visitor<'de> for BinaryDataVisitor {
            type Value = BinaryData<'de>;

            fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(formatter, "BinaryData")
            }

            fn visit_borrowed_bytes<E: serde::de::Error>(
                self,
                data: &'de [u8],
            ) -> Result<Self::Value, E> {
                Ok(BinaryData(data))
            }
        }

        deserializer.deserialize_bytes(BinaryDataVisitor)
    }
}

/// A wrapper type for "UTF-8 Encoded Strings" as defined in the MQTT v5 specification.
///
/// # Note
/// This wrapper type is primarily used to support custom serde functionality.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Utf8String<'a>(pub &'a str);

impl<'a> serde::Serialize for Utf8String<'a> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        if self.0.len() > u16::MAX as usize {
            return Err(S::Error::custom("Provided string is too long"));
        }

        let len = self.0.len() as u16;
        let mut item = serializer.serialize_struct("_Utf8String", 0)?;

        // UTF-8 encoded strings in MQTT require a u16 length prefix to indicate their length.
        item.serialize_field("_len", &len)?;
        item.serialize_field("_string", self.0)?;
        item.end()
    }
}

impl<'a, 'de: 'a> serde::de::Deserialize<'de> for Utf8String<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Utf8StringVisitor<'a> {
            _data: core::marker::PhantomData<&'a ()>,
        }

        impl<'a, 'de: 'a> serde::de::Visitor<'de> for Utf8StringVisitor<'a> {
            type Value = Utf8String<'a>;

            fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
                write!(formatter, "Utf8String")
            }

            fn visit_borrowed_str<E: serde::de::Error>(
                self,
                data: &'de str,
            ) -> Result<Self::Value, E> {
                Ok(Utf8String(data))
            }
        }

        // The UTF-8 string in MQTTv5 is semantically equivalent to a rust &str.
        deserializer.deserialize_str(Utf8StringVisitor {
            _data: core::marker::PhantomData::default(),
        })
    }
}

/// A wrapper type for "Subscription Options" as defined in the MQTT v5 specification.
///
/// # Note
/// This wrapper type is primarily used to support custom serde functionality.
#[derive(Copy, Clone, Debug)]
pub struct SubscriptionOptions {}

impl serde::Serialize for SubscriptionOptions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // TODO: Support subscription options
        serializer.serialize_u8(0)
    }
}
