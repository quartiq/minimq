use crate::{ser::ReversedPacketWriter, varint::Varint, ProtocolError as Error};

use core::convert::TryFrom;
use num_enum::TryFromPrimitive;

#[derive(Debug, Copy, Clone, PartialEq, TryFromPrimitive)]
#[repr(u32)]
pub(crate) enum PropertyIdentifier {
    Invalid = u32::MAX,

    PayloadFormatIndicator = 0x01,
    MessageExpiryInterval = 0x02,
    ContentType = 0x03,

    ResponseTopic = 0x08,
    CorrelationData = 0x09,

    SubscriptionIdentifier = 0x0B,

    SessionExpiryInterval = 0x11,
    AssignedClientIdentifier = 0x12,
    ServerKeepAlive = 0x13,
    AuthenticationMethod = 0x15,
    AuthenticationData = 0x16,
    RequestProblemInformation = 0x17,
    WillDelayInterval = 0x18,
    RequestResponseInformation = 0x19,

    ResponseInformation = 0x1A,

    ServerReference = 0x1C,

    ReasonString = 0x1F,

    ReceiveMaximum = 0x21,
    TopicAliasMaximum = 0x22,
    TopicAlias = 0x23,
    MaximumQoS = 0x24,
    RetainAvailable = 0x25,
    UserProperty = 0x26,
    MaximumPacketSize = 0x27,
    WildcardSubscriptionAvailable = 0x28,
    SubscriptionIdentifierAvailable = 0x29,
    SharedSubscriptionAvailable = 0x2A,
}

struct PropertyIdVisitor;

impl<'de> serde::de::Visitor<'de> for PropertyIdVisitor {
    type Value = PropertyIdentifier;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "PropertyIdentifier")
    }

    fn visit_u32<E: serde::de::Error>(self, v: u32) -> Result<Self::Value, E> {
        PropertyIdentifier::try_from(v).map_err(|_| E::custom("Invalid PropertyIdentifier"))
    }
}

impl<'de> serde::de::Deserialize<'de> for PropertyIdentifier {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let result = deserializer.deserialize_u32(PropertyIdVisitor)?;
        Ok(result)
    }
}

/// All of the possible properties that MQTT version 5 supports.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum Property<'a> {
    PayloadFormatIndicator(u8),
    MessageExpiryInterval(u32),
    ContentType(&'a str),
    ResponseTopic(&'a str),
    CorrelationData(&'a [u8]),
    SubscriptionIdentifier(Varint),
    SessionExpiryInterval(u32),
    AssignedClientIdentifier(&'a str),
    ServerKeepAlive(u16),
    AuthenticationMethod(&'a str),
    AuthenticationData(&'a [u8]),
    RequestProblemInformation(u8),
    WillDelayInterval(u32),
    RequestResponseInformation(u8),
    ResponseInformation(&'a str),
    ServerReference(&'a str),
    ReasonString(&'a str),
    ReceiveMaximum(u16),
    TopicAliasMaximum(u16),
    TopicAlias(u16),
    MaximumQoS(u8),
    RetainAvailable(u8),
    UserProperty(&'a str, &'a str),
    MaximumPacketSize(u32),
    WildcardSubscriptionAvailable(u8),
    SubscriptionIdentifierAvailable(u8),
    SharedSubscriptionAvailable(u8),
}

struct UserPropertyVisitor<'a> {
    _data: core::marker::PhantomData<&'a ()>,
}

impl<'a, 'de: 'a> serde::de::Visitor<'de> for UserPropertyVisitor<'a> {
    type Value = (&'a str, &'a str);

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "UserProperty")
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
        use serde::de::Error;
        let key = seq
            .next_element()?
            .ok_or_else(|| A::Error::custom("No key present"))?;
        let value = seq
            .next_element()?
            .ok_or_else(|| A::Error::custom("No value present"))?;
        Ok((key, value))
    }
}

struct PropertyVisitor<'a> {
    _data: core::marker::PhantomData<&'a ()>,
}

impl<'a, 'de: 'a> serde::de::Visitor<'de> for PropertyVisitor<'a> {
    type Value = Property<'a>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "enum Property")
    }

    fn visit_enum<A: serde::de::EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
        use serde::de::{Error, VariantAccess};

        let (field, variant) = data.variant::<PropertyIdentifier>()?;
        crate::trace!("Deserializing {:?}", field);

        let property = match field {
            PropertyIdentifier::ResponseTopic => {
                Property::ResponseTopic(variant.newtype_variant()?)
            }
            PropertyIdentifier::PayloadFormatIndicator => {
                Property::PayloadFormatIndicator(variant.newtype_variant()?)
            }
            PropertyIdentifier::MessageExpiryInterval => {
                Property::MessageExpiryInterval(variant.newtype_variant()?)
            }
            PropertyIdentifier::ContentType => Property::ContentType(variant.newtype_variant()?),
            PropertyIdentifier::CorrelationData => {
                Property::CorrelationData(variant.newtype_variant()?)
            }
            PropertyIdentifier::SubscriptionIdentifier => {
                Property::SubscriptionIdentifier(variant.newtype_variant()?)
            }
            PropertyIdentifier::SessionExpiryInterval => {
                Property::SessionExpiryInterval(variant.newtype_variant()?)
            }
            PropertyIdentifier::AssignedClientIdentifier => {
                Property::AssignedClientIdentifier(variant.newtype_variant()?)
            }
            PropertyIdentifier::ServerKeepAlive => {
                Property::ServerKeepAlive(variant.newtype_variant()?)
            }
            PropertyIdentifier::AuthenticationMethod => {
                Property::AuthenticationMethod(variant.newtype_variant()?)
            }
            PropertyIdentifier::AuthenticationData => {
                Property::AuthenticationData(variant.newtype_variant()?)
            }
            PropertyIdentifier::RequestProblemInformation => {
                Property::RequestProblemInformation(variant.newtype_variant()?)
            }
            PropertyIdentifier::WillDelayInterval => {
                Property::WillDelayInterval(variant.newtype_variant()?)
            }
            PropertyIdentifier::RequestResponseInformation => {
                Property::RequestResponseInformation(variant.newtype_variant()?)
            }
            PropertyIdentifier::ResponseInformation => {
                Property::ResponseInformation(variant.newtype_variant()?)
            }
            PropertyIdentifier::ServerReference => {
                Property::ServerReference(variant.newtype_variant()?)
            }
            PropertyIdentifier::ReasonString => Property::ReasonString(variant.newtype_variant()?),
            PropertyIdentifier::ReceiveMaximum => {
                Property::ReceiveMaximum(variant.newtype_variant()?)
            }
            PropertyIdentifier::TopicAliasMaximum => {
                Property::TopicAliasMaximum(variant.newtype_variant()?)
            }
            PropertyIdentifier::TopicAlias => Property::TopicAlias(variant.newtype_variant()?),
            PropertyIdentifier::MaximumQoS => Property::MaximumQoS(variant.newtype_variant()?),
            PropertyIdentifier::RetainAvailable => {
                Property::RetainAvailable(variant.newtype_variant()?)
            }
            PropertyIdentifier::UserProperty => {
                let (key, value) = variant.tuple_variant(
                    2,
                    UserPropertyVisitor {
                        _data: core::marker::PhantomData::default(),
                    },
                )?;
                Property::UserProperty(key, value)
            }
            PropertyIdentifier::MaximumPacketSize => {
                Property::MaximumPacketSize(variant.newtype_variant()?)
            }
            PropertyIdentifier::WildcardSubscriptionAvailable => {
                Property::WildcardSubscriptionAvailable(variant.newtype_variant()?)
            }
            PropertyIdentifier::SubscriptionIdentifierAvailable => {
                Property::SubscriptionIdentifierAvailable(variant.newtype_variant()?)
            }
            PropertyIdentifier::SharedSubscriptionAvailable => {
                Property::SharedSubscriptionAvailable(variant.newtype_variant()?)
            }

            _ => return Err(A::Error::custom("Invalid property identifier")),
        };

        Ok(property)
    }
}

impl<'a, 'de: 'a> serde::de::Deserialize<'de> for Property<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let prop = deserializer.deserialize_enum(
            "Property",
            &[],
            PropertyVisitor {
                _data: core::marker::PhantomData::default(),
            },
        )?;
        crate::debug!("Deserialized {:?}", prop);
        Ok(prop)
    }
}

impl<'a> From<&Property<'a>> for PropertyIdentifier {
    fn from(prop: &Property<'a>) -> PropertyIdentifier {
        match prop {
            Property::PayloadFormatIndicator(_) => PropertyIdentifier::PayloadFormatIndicator,
            Property::MessageExpiryInterval(_) => PropertyIdentifier::MessageExpiryInterval,
            Property::ContentType(_) => PropertyIdentifier::ContentType,
            Property::ResponseTopic(_) => PropertyIdentifier::ResponseTopic,
            Property::CorrelationData(_) => PropertyIdentifier::CorrelationData,
            Property::SubscriptionIdentifier(_) => PropertyIdentifier::SubscriptionIdentifier,
            Property::SessionExpiryInterval(_) => PropertyIdentifier::SessionExpiryInterval,
            Property::AssignedClientIdentifier(_) => PropertyIdentifier::AssignedClientIdentifier,
            Property::ServerKeepAlive(_) => PropertyIdentifier::ServerKeepAlive,
            Property::AuthenticationMethod(_) => PropertyIdentifier::AuthenticationMethod,
            Property::AuthenticationData(_) => PropertyIdentifier::AuthenticationData,
            Property::RequestProblemInformation(_) => PropertyIdentifier::RequestProblemInformation,
            Property::WillDelayInterval(_) => PropertyIdentifier::WillDelayInterval,
            Property::RequestResponseInformation(_) => {
                PropertyIdentifier::RequestResponseInformation
            }
            Property::ResponseInformation(_) => PropertyIdentifier::ResponseInformation,
            Property::ServerReference(_) => PropertyIdentifier::ServerReference,
            Property::ReasonString(_) => PropertyIdentifier::ReasonString,
            Property::ReceiveMaximum(_) => PropertyIdentifier::ReceiveMaximum,
            Property::TopicAliasMaximum(_) => PropertyIdentifier::TopicAliasMaximum,
            Property::TopicAlias(_) => PropertyIdentifier::TopicAlias,
            Property::MaximumQoS(_) => PropertyIdentifier::MaximumQoS,
            Property::RetainAvailable(_) => PropertyIdentifier::RetainAvailable,
            Property::UserProperty(_, _) => PropertyIdentifier::UserProperty,
            Property::MaximumPacketSize(_) => PropertyIdentifier::MaximumPacketSize,
            Property::WildcardSubscriptionAvailable(_) => {
                PropertyIdentifier::WildcardSubscriptionAvailable
            }
            Property::SubscriptionIdentifierAvailable(_) => {
                PropertyIdentifier::SubscriptionIdentifierAvailable
            }
            Property::SharedSubscriptionAvailable(_) => {
                PropertyIdentifier::SharedSubscriptionAvailable
            }
        }
    }
}

impl<'a> Property<'a> {
    pub(crate) fn encode_into<'b>(
        &self,
        packet: &mut ReversedPacketWriter<'b>,
    ) -> Result<(), Error> {
        match self {
            Property::PayloadFormatIndicator(value) => packet.write_u8(*value)?,
            Property::MessageExpiryInterval(value) => packet.write_u32(*value)?,
            Property::ContentType(content_type) => packet.write_utf8_string(content_type)?,
            Property::ResponseTopic(topic) => packet.write_utf8_string(topic)?,
            Property::CorrelationData(data) => packet.write_binary_data(data)?,
            Property::SubscriptionIdentifier(data) => {
                packet.write_variable_length_integer(data.0)?
            }
            Property::SessionExpiryInterval(data) => packet.write_u32(*data)?,
            Property::AssignedClientIdentifier(data) => packet.write_utf8_string(data)?,
            Property::ServerKeepAlive(data) => packet.write_u16(*data)?,
            Property::AuthenticationMethod(data) => packet.write_utf8_string(data)?,
            Property::AuthenticationData(data) => packet.write_binary_data(data)?,
            Property::RequestProblemInformation(data) => packet.write_u8(*data)?,
            Property::WillDelayInterval(data) => packet.write_u32(*data)?,
            Property::RequestResponseInformation(data) => packet.write_u8(*data)?,
            Property::ResponseInformation(data) => packet.write_utf8_string(data)?,
            Property::ServerReference(data) => packet.write_utf8_string(data)?,
            Property::ReasonString(data) => packet.write_utf8_string(data)?,
            Property::ReceiveMaximum(data) => packet.write_u16(*data)?,
            Property::TopicAliasMaximum(data) => packet.write_u16(*data)?,
            Property::TopicAlias(data) => packet.write_u16(*data)?,
            Property::MaximumQoS(data) => packet.write_u8(*data)?,
            Property::RetainAvailable(data) => packet.write_u8(*data)?,
            Property::UserProperty(key, value) => {
                packet.write_utf8_string(value)?;
                packet.write_utf8_string(key)?;
            }
            Property::MaximumPacketSize(data) => packet.write_u32(*data)?,
            Property::WildcardSubscriptionAvailable(data) => packet.write_u8(*data)?,
            Property::SubscriptionIdentifierAvailable(data) => packet.write_u8(*data)?,
            Property::SharedSubscriptionAvailable(data) => packet.write_u8(*data)?,
        };

        let id: PropertyIdentifier = self.into();
        packet.write_variable_length_integer(id as u32)
    }
}
