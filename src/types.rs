//! MQTT-Specific Data Types
//!
//! This module provides wrapper methods and serde functionality for MQTT-specified data types.
use crate::{properties::Property, varint::Varint, QoS};
use bit_field::BitField;
use serde::ser::SerializeStruct;
use serde::Serialize;

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
        let property_length: usize = self.0.iter().map(|prop| prop.size()).sum();
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

struct BinaryDataVisitor;

impl<'de> serde::de::Visitor<'de> for BinaryDataVisitor {
    type Value = BinaryData<'de>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "BinaryData")
    }

    fn visit_borrowed_bytes<E: serde::de::Error>(self, data: &'de [u8]) -> Result<Self::Value, E> {
        Ok(BinaryData(data))
    }
}

impl<'de> serde::de::Deserialize<'de> for BinaryData<'de> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
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

struct Utf8StringVisitor<'a> {
    _data: core::marker::PhantomData<&'a ()>,
}

impl<'a, 'de: 'a> serde::de::Visitor<'de> for Utf8StringVisitor<'a> {
    type Value = Utf8String<'a>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "Utf8String")
    }

    fn visit_borrowed_str<E: serde::de::Error>(self, data: &'de str) -> Result<Self::Value, E> {
        Ok(Utf8String(data))
    }
}

impl<'a, 'de: 'a> serde::de::Deserialize<'de> for Utf8String<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // The UTF-8 string in MQTTv5 is semantically equivalent to a rust &str.
        deserializer.deserialize_str(Utf8StringVisitor {
            _data: core::marker::PhantomData,
        })
    }
}

/// Used to specify how currently-retained messages should be handled after the topic is subscribed to.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum RetainHandling {
    /// All retained messages should immediately be transmitted if they are present.
    Immediately = 0b00,

    /// Retained messages should only be published if the subscription does not already exist.
    IfSubscriptionDoesNotExist = 0b01,

    /// Do not provide any retained messages on this topic.
    Never = 0b10,
}

/// A wrapper type for "Subscription Options" as defined in the MQTT v5 specification.
///
/// # Note
/// This wrapper type is primarily used to support custom serde functionality.
#[derive(Copy, Clone, Debug)]
pub struct SubscriptionOptions {
    maximum_qos: QoS,
    no_local: bool,
    retain_as_published: bool,
    retain_behavior: RetainHandling,
}

impl Default for SubscriptionOptions {
    fn default() -> Self {
        Self {
            maximum_qos: QoS::AtMostOnce,
            no_local: false,
            retain_as_published: false,
            retain_behavior: RetainHandling::Immediately,
        }
    }
}

impl SubscriptionOptions {
    /// Specify the maximum QoS supported on this subscription.
    pub fn maximum_qos(mut self, qos: QoS) -> Self {
        // TODO: Support for higher QoS levels.
        assert!(qos == QoS::AtMostOnce);
        self.maximum_qos = qos;
        self
    }

    /// Specify the retain behavior of the topic subscription.
    pub fn retain_behavior(mut self, handling: RetainHandling) -> Self {
        self.retain_behavior = handling;
        self
    }

    /// Ignore locally-published messages on this subscription.
    pub fn ignore_local_messages(mut self) -> Self {
        self.no_local = true;
        self
    }

    /// Keep the retain bits unchanged for this subscription.
    pub fn retain_as_published(mut self) -> Self {
        self.retain_as_published = true;
        self
    }
}

impl serde::Serialize for SubscriptionOptions {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let value = *0u8
            .set_bits(0..2, self.maximum_qos as u8)
            .set_bit(2, self.no_local)
            .set_bit(3, self.retain_as_published)
            .set_bits(4..6, self.retain_behavior as u8);
        serializer.serialize_u8(value)
    }
}

impl<'a> From<&'a str> for TopicFilter<'a> {
    fn from(topic: &'a str) -> Self {
        Self {
            topic: Utf8String(topic),
            options: SubscriptionOptions::default(),
        }
    }
}

/// A single topic subscription.
///
/// # Note
/// Many topic filters may be requested in a single subscription request.
#[derive(Serialize, Copy, Clone, Debug)]
pub struct TopicFilter<'a> {
    topic: Utf8String<'a>,
    options: SubscriptionOptions,
}

impl<'a> TopicFilter<'a> {
    /// Create a new topic filter for subscription.
    pub fn new(topic: &'a str) -> Self {
        topic.into()
    }

    /// Specify custom options for the subscription.
    pub fn options(mut self, options: SubscriptionOptions) -> Self {
        self.options = options;
        self
    }
}
