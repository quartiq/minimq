use crate::{
    types::{BinaryData, Utf8String},
    varint::Varint,
};

use core::convert::TryFrom;
use num_enum::TryFromPrimitive;
use serde::ser::SerializeSeq;

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
    ContentType(Utf8String<'a>),
    ResponseTopic(Utf8String<'a>),
    CorrelationData(BinaryData<'a>),
    SubscriptionIdentifier(Varint),
    SessionExpiryInterval(u32),
    AssignedClientIdentifier(Utf8String<'a>),
    ServerKeepAlive(u16),
    AuthenticationMethod(Utf8String<'a>),
    AuthenticationData(BinaryData<'a>),
    RequestProblemInformation(u8),
    WillDelayInterval(u32),
    RequestResponseInformation(u8),
    ResponseInformation(Utf8String<'a>),
    ServerReference(Utf8String<'a>),
    ReasonString(Utf8String<'a>),
    ReceiveMaximum(u16),
    TopicAliasMaximum(u16),
    TopicAlias(u16),
    MaximumQoS(u8),
    RetainAvailable(u8),
    UserProperty(Utf8String<'a>, Utf8String<'a>),
    MaximumPacketSize(u32),
    WildcardSubscriptionAvailable(u8),
    SubscriptionIdentifierAvailable(u8),
    SharedSubscriptionAvailable(u8),
}

struct UserPropertyVisitor<'a> {
    _data: core::marker::PhantomData<&'a ()>,
}

impl<'a, 'de: 'a> serde::de::Visitor<'de> for UserPropertyVisitor<'a> {
    type Value = (Utf8String<'a>, Utf8String<'a>);

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

impl<'a> serde::Serialize for Property<'a> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut serializer = serializer.serialize_seq(None)?;

        let id: PropertyIdentifier = self.into();
        serializer.serialize_element(&Varint(id as u32))?;

        match self {
            Property::PayloadFormatIndicator(value) => serializer.serialize_element(value)?,
            Property::MessageExpiryInterval(value) => serializer.serialize_element(value)?,
            Property::ContentType(content_type) => serializer.serialize_element(content_type)?,
            Property::ResponseTopic(topic) => serializer.serialize_element(topic)?,
            Property::CorrelationData(data) => serializer.serialize_element(data)?,
            Property::SubscriptionIdentifier(data) => serializer.serialize_element(data)?,
            Property::SessionExpiryInterval(data) => serializer.serialize_element(data)?,
            Property::AssignedClientIdentifier(data) => serializer.serialize_element(data)?,
            Property::ServerKeepAlive(data) => serializer.serialize_element(data)?,
            Property::AuthenticationMethod(data) => serializer.serialize_element(data)?,
            Property::AuthenticationData(data) => serializer.serialize_element(data)?,
            Property::RequestProblemInformation(data) => serializer.serialize_element(data)?,
            Property::WillDelayInterval(data) => serializer.serialize_element(data)?,
            Property::RequestResponseInformation(data) => serializer.serialize_element(data)?,
            Property::ResponseInformation(data) => serializer.serialize_element(data)?,
            Property::ServerReference(data) => serializer.serialize_element(data)?,
            Property::ReasonString(data) => serializer.serialize_element(data)?,
            Property::ReceiveMaximum(data) => serializer.serialize_element(data)?,
            Property::TopicAliasMaximum(data) => serializer.serialize_element(data)?,
            Property::TopicAlias(data) => serializer.serialize_element(data)?,
            Property::MaximumQoS(data) => serializer.serialize_element(data)?,
            Property::RetainAvailable(data) => serializer.serialize_element(data)?,
            Property::UserProperty(key, value) => {
                serializer.serialize_element(value)?;
                serializer.serialize_element(key)?;
            }
            Property::MaximumPacketSize(data) => serializer.serialize_element(data)?,
            Property::WildcardSubscriptionAvailable(data) => serializer.serialize_element(data)?,
            Property::SubscriptionIdentifierAvailable(data) => {
                serializer.serialize_element(data)?
            }
            Property::SharedSubscriptionAvailable(data) => serializer.serialize_element(data)?,
        }

        serializer.end()
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
    pub(crate) fn size(&self) -> usize {
        // Although property identifiers are technically encoded as variable length integers, in
        // practice, they are all small enough to fit in 1 byte.
        let identifier_length = 1;

        match self {
            Property::ContentType(data)
            | Property::ResponseTopic(data)
            | Property::AuthenticationMethod(data)
            | Property::ResponseInformation(data)
            | Property::ServerReference(data)
            | Property::ReasonString(data)
            | Property::AssignedClientIdentifier(data) => data.0.len() + 2 + identifier_length,
            Property::UserProperty(key, value) => {
                (value.0.len() + 2) + (key.0.len() + 2) + identifier_length
            }
            Property::CorrelationData(data) | Property::AuthenticationData(data) => {
                data.0.len() + 2 + identifier_length
            }
            Property::SubscriptionIdentifier(id) => id.len() + identifier_length,

            Property::MessageExpiryInterval(_)
            | Property::SessionExpiryInterval(_)
            | Property::WillDelayInterval(_)
            | Property::MaximumPacketSize(_) => 4 + identifier_length,
            Property::ServerKeepAlive(_)
            | Property::ReceiveMaximum(_)
            | Property::TopicAliasMaximum(_)
            | Property::TopicAlias(_) => 2 + identifier_length,
            Property::PayloadFormatIndicator(_)
            | Property::RequestProblemInformation(_)
            | Property::RequestResponseInformation(_)
            | Property::MaximumQoS(_)
            | Property::RetainAvailable(_)
            | Property::WildcardSubscriptionAvailable(_)
            | Property::SubscriptionIdentifierAvailable(_)
            | Property::SharedSubscriptionAvailable(_) => 1 + identifier_length,
        }
    }
}
