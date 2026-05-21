use crate::{
    PeerError,
    de::deserializer::MqttDeserializer,
    varint::Varint,
    wire::{BinaryData, Utf8String},
};

use core::convert::TryFrom;
use num_enum::TryFromPrimitive;
use serde::Deserialize;
use serde::ser::{SerializeSeq, SerializeStruct};

#[derive(Debug, Copy, Clone, PartialEq, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

impl serde::de::Visitor<'_> for PropertyIdVisitor {
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
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Property<'a> {
    /// Payload format indicator.
    PayloadFormatIndicator(u8),
    /// Message expiry interval in seconds.
    MessageExpiryInterval(u32),
    /// Content type of the application payload.
    ContentType(&'a str),
    /// Response topic for request/reply patterns.
    ResponseTopic(&'a str),
    /// Correlation data for request/reply patterns.
    CorrelationData(&'a [u8]),
    /// Subscription identifier.
    SubscriptionIdentifier(u32),
    /// Session expiry interval in seconds.
    SessionExpiryInterval(u32),
    /// Client identifier assigned by the broker.
    AssignedClientIdentifier(&'a str),
    /// Broker-advertised keepalive.
    ServerKeepAlive(u16),
    /// Authentication method name.
    AuthenticationMethod(&'a str),
    /// Authentication data bytes.
    AuthenticationData(&'a [u8]),
    /// Whether problem information is requested.
    RequestProblemInformation(u8),
    /// Delay before publishing the will, in seconds.
    WillDelayInterval(u32),
    /// Whether response information is requested.
    RequestResponseInformation(u8),
    /// Broker response information.
    ResponseInformation(&'a str),
    /// Alternate broker reference.
    ServerReference(&'a str),
    /// Human-readable reason string.
    ReasonString(&'a str),
    /// Maximum concurrent QoS 1/2 receives.
    ReceiveMaximum(u16),
    /// Maximum supported topic alias.
    TopicAliasMaximum(u16),
    /// Topic alias value.
    TopicAlias(u16),
    /// Maximum supported QoS.
    MaximumQoS(u8),
    /// Whether retained messages are supported.
    RetainAvailable(u8),
    /// User-defined key/value property.
    UserProperty(&'a str, &'a str),
    /// Maximum packet size in bytes.
    MaximumPacketSize(u32),
    /// Whether wildcard subscriptions are supported.
    WildcardSubscriptionAvailable(u8),
    /// Whether subscription identifiers are supported.
    SubscriptionIdentifierAvailable(u8),
    /// Whether shared subscriptions are supported.
    SharedSubscriptionAvailable(u8),
}

#[derive(Debug, Copy, Clone)]
pub(crate) enum PropertyContext {
    Publish,
    Subscribe,
    Unsubscribe,
    Disconnect,
    Will,
}

impl Property<'_> {
    fn has_valid_value(&self) -> bool {
        match self {
            Property::PayloadFormatIndicator(value)
            | Property::RequestProblemInformation(value)
            | Property::RequestResponseInformation(value)
            | Property::RetainAvailable(value)
            | Property::WildcardSubscriptionAvailable(value)
            | Property::SubscriptionIdentifierAvailable(value)
            | Property::SharedSubscriptionAvailable(value) => *value <= 1,
            Property::MaximumQoS(value) => *value <= 2,
            Property::SubscriptionIdentifier(value) => {
                (1..=crate::varint::MQTT_VARINT_MAX).contains(value)
            }
            _ => true,
        }
    }

    pub(crate) fn is_valid_for(&self, context: PropertyContext) -> bool {
        if !self.has_valid_value() {
            return false;
        }

        matches!(
            (context, self.into()),
            (
                PropertyContext::Publish | PropertyContext::Will,
                PropertyIdentifier::PayloadFormatIndicator
                    | PropertyIdentifier::MessageExpiryInterval
                    | PropertyIdentifier::ContentType
                    | PropertyIdentifier::ResponseTopic
                    | PropertyIdentifier::CorrelationData
                    | PropertyIdentifier::UserProperty,
            ) | (PropertyContext::Publish, PropertyIdentifier::TopicAlias)
                | (
                    PropertyContext::Subscribe,
                    PropertyIdentifier::SubscriptionIdentifier | PropertyIdentifier::UserProperty,
                )
                | (
                    PropertyContext::Unsubscribe,
                    PropertyIdentifier::UserProperty
                )
                | (
                    PropertyContext::Disconnect,
                    PropertyIdentifier::SessionExpiryInterval
                        | PropertyIdentifier::ReasonString
                        | PropertyIdentifier::UserProperty
                        | PropertyIdentifier::ServerReference,
                )
        )
    }
}

/// MQTT property collection attached to a packet.
///
/// Application code usually receives this from inbound packets or passes borrowed property slices
/// to packet builders. The storage representation is intentionally opaque: properties may be
/// borrowed decoded values, borrowed encoded broker data, or a small synthetic view.
#[derive(Debug, PartialEq)]
pub struct Properties<'a> {
    inner: PropertiesData<'a>,
}

#[derive(Debug, PartialEq)]
enum PropertiesData<'a> {
    Slice(&'a [Property<'a>]),
    Encoded(&'a [u8]),
    WithCorrelation {
        correlation: Property<'a>,
        properties: &'a [Property<'a>],
    },
}

impl Properties<'_> {
    /// Return an empty property collection.
    pub const fn empty() -> Self {
        Self::from_slice(&[])
    }

    /// Borrow a decoded property slice.
    pub const fn from_slice<'a>(properties: &'a [Property<'a>]) -> Properties<'a> {
        Properties {
            inner: PropertiesData::Slice(properties),
        }
    }

    /// Return the encoded MQTT property block size in bytes.
    pub fn size(&self) -> usize {
        match &self.inner {
            PropertiesData::Slice(props) => props.iter().map(|prop| prop.size()).sum(),
            PropertiesData::WithCorrelation {
                correlation,
                properties,
            } => properties
                .iter()
                .chain([correlation.clone()].iter())
                .map(|prop| prop.size())
                .sum(),
            PropertiesData::Encoded(block) => block.len(),
        }
    }
}

impl<'a> Properties<'a> {
    pub(crate) const fn encoded(data: &'a [u8]) -> Self {
        Self {
            inner: PropertiesData::Encoded(data),
        }
    }

    /// Iterate over properties.
    pub fn iter(&'a self) -> impl Iterator<Item = Result<Property<'a>, PeerError>> + 'a {
        self.iter_inner()
    }

    /// Return the first `ResponseTopic` property, if present.
    pub fn response_topic(&'a self) -> Option<&'a str> {
        self.iter().find_map(|prop| match prop {
            Ok(crate::Property::ResponseTopic(topic)) => Some(topic),
            _ => None,
        })
    }

    /// Return the first `CorrelationData` property, if present.
    pub fn correlation_data(&'a self) -> Option<&'a [u8]> {
        self.iter().find_map(|prop| match prop {
            Ok(crate::Property::CorrelationData(data)) => Some(data),
            _ => None,
        })
    }

    pub(crate) fn with_properties(self, properties: &'a [Property<'a>]) -> Self {
        match self.inner {
            PropertiesData::WithCorrelation { correlation, .. } => Self {
                inner: PropertiesData::WithCorrelation {
                    correlation,
                    properties,
                },
            },
            PropertiesData::Slice(_) | PropertiesData::Encoded(_) => Self::from_slice(properties),
        }
    }

    pub(crate) fn with_correlation(self, data: &'a [u8]) -> Self {
        let correlation = Property::CorrelationData(data);
        match self.inner {
            PropertiesData::Slice(properties)
            | PropertiesData::WithCorrelation { properties, .. } => Self {
                inner: PropertiesData::WithCorrelation {
                    correlation,
                    properties,
                },
            },
            PropertiesData::Encoded(_) => Self {
                inner: PropertiesData::WithCorrelation {
                    correlation,
                    properties: &[],
                },
            },
        }
    }

    pub(crate) fn iter_decoded(&'a self) -> PropertiesIter<'a> {
        self.iter_inner()
    }

    pub(crate) fn valid_for(&'a self, context: PropertyContext) -> bool {
        self.iter()
            .all(|property| property.is_ok_and(|property| property.is_valid_for(context)))
    }

    fn iter_inner(&'a self) -> PropertiesIter<'a> {
        match &self.inner {
            PropertiesData::Encoded(data) => PropertiesIter {
                inner: PropertiesIterInner::Encoded {
                    props: data,
                    index: 0,
                },
            },
            PropertiesData::Slice(props) => PropertiesIter {
                inner: PropertiesIterInner::Slice { props, index: 0 },
            },
            PropertiesData::WithCorrelation {
                correlation,
                properties,
            } => PropertiesIter {
                inner: PropertiesIterInner::WithCorrelation {
                    correlation: correlation.clone(),
                    yielded_correlation: false,
                    props: properties,
                    index: 0,
                },
            },
        }
    }
}

/// Iterator over decoded MQTT properties.
pub(crate) struct PropertiesIter<'a> {
    inner: PropertiesIterInner<'a>,
}

enum PropertiesIterInner<'a> {
    Encoded {
        props: &'a [u8],
        index: usize,
    },
    Slice {
        props: &'a [Property<'a>],
        index: usize,
    },
    WithCorrelation {
        correlation: Property<'a>,
        yielded_correlation: bool,
        props: &'a [Property<'a>],
        index: usize,
    },
}

impl<'a> core::iter::Iterator for PropertiesIter<'a> {
    type Item = Result<Property<'a>, PeerError>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            PropertiesIterInner::Encoded { props, index } => {
                if *index >= props.len() {
                    return None;
                }

                let mut deserializer = MqttDeserializer::new(&props[*index..]);
                let property =
                    Property::deserialize(&mut deserializer).map_err(|_| PeerError::InvalidPacket);
                *index += deserializer.deserialized_bytes();
                Some(property)
            }
            PropertiesIterInner::Slice { props, index } => {
                let property = props.get(*index).cloned()?;
                *index += 1;
                Some(Ok(property))
            }
            PropertiesIterInner::WithCorrelation {
                correlation,
                yielded_correlation,
                props,
                index,
            } => {
                if !*yielded_correlation {
                    *yielded_correlation = true;
                    return Some(Ok(correlation.clone()));
                }

                let property = props.get(*index).cloned()?;
                *index += 1;
                Some(Ok(property))
            }
        }
    }
}

impl serde::Serialize for Properties<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut item = serializer.serialize_struct("Properties", 0)?;
        item.serialize_field("_len", &Varint(self.size() as u32))?;

        match &self.inner {
            PropertiesData::Slice(props) => {
                item.serialize_field("_props", props)?;
            }
            PropertiesData::WithCorrelation {
                correlation,
                properties,
            } => {
                item.serialize_field("_correlation", &correlation)?;
                item.serialize_field("_props", properties)?;
            }
            PropertiesData::Encoded(block) => {
                item.serialize_field("_data", block)?;
            }
        }

        item.end()
    }
}

struct PropertiesVisitor;

impl<'de> serde::de::Visitor<'de> for PropertiesVisitor {
    type Value = Properties<'de>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "Properties")
    }

    fn visit_seq<S: serde::de::SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        let data = seq.next_element()?;
        Ok(Properties::encoded(data.unwrap_or(&[])))
    }
}

impl<'a, 'de: 'a> serde::de::Deserialize<'de> for Properties<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(PropertiesVisitor)
    }
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
        let key: Utf8String<'a> = key;
        let value: Utf8String<'a> = value;
        Ok((key.0, value.0))
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
        crate::trace!("Deserializing property field {}", field);

        let property = match field {
            PropertyIdentifier::ResponseTopic => {
                let value: Utf8String<'a> = variant.newtype_variant()?;
                Property::ResponseTopic(value.0)
            }
            PropertyIdentifier::PayloadFormatIndicator => {
                Property::PayloadFormatIndicator(variant.newtype_variant()?)
            }
            PropertyIdentifier::MessageExpiryInterval => {
                Property::MessageExpiryInterval(variant.newtype_variant()?)
            }
            PropertyIdentifier::ContentType => {
                let value: Utf8String<'a> = variant.newtype_variant()?;
                Property::ContentType(value.0)
            }
            PropertyIdentifier::CorrelationData => {
                let value: BinaryData<'a> = variant.newtype_variant()?;
                Property::CorrelationData(value.0)
            }
            PropertyIdentifier::SubscriptionIdentifier => {
                let value: Varint = variant.newtype_variant()?;
                Property::SubscriptionIdentifier(value.0)
            }
            PropertyIdentifier::SessionExpiryInterval => {
                Property::SessionExpiryInterval(variant.newtype_variant()?)
            }
            PropertyIdentifier::AssignedClientIdentifier => {
                let value: Utf8String<'a> = variant.newtype_variant()?;
                Property::AssignedClientIdentifier(value.0)
            }
            PropertyIdentifier::ServerKeepAlive => {
                Property::ServerKeepAlive(variant.newtype_variant()?)
            }
            PropertyIdentifier::AuthenticationMethod => {
                let value: Utf8String<'a> = variant.newtype_variant()?;
                Property::AuthenticationMethod(value.0)
            }
            PropertyIdentifier::AuthenticationData => {
                let value: BinaryData<'a> = variant.newtype_variant()?;
                Property::AuthenticationData(value.0)
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
                let value: Utf8String<'a> = variant.newtype_variant()?;
                Property::ResponseInformation(value.0)
            }
            PropertyIdentifier::ServerReference => {
                let value: Utf8String<'a> = variant.newtype_variant()?;
                Property::ServerReference(value.0)
            }
            PropertyIdentifier::ReasonString => {
                let value: Utf8String<'a> = variant.newtype_variant()?;
                Property::ReasonString(value.0)
            }
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
                        _data: core::marker::PhantomData,
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
                _data: core::marker::PhantomData,
            },
        )?;
        crate::trace!("Deserialized property {}", prop);
        Ok(prop)
    }
}

impl serde::Serialize for Property<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut serializer = serializer.serialize_seq(None)?;

        let id: PropertyIdentifier = self.into();
        serializer.serialize_element(&Varint(id as u32))?;

        match self {
            Property::PayloadFormatIndicator(value) => serializer.serialize_element(value)?,
            Property::MessageExpiryInterval(value) => serializer.serialize_element(value)?,
            Property::ContentType(content_type) => {
                serializer.serialize_element(&Utf8String(content_type))?;
            }
            Property::ResponseTopic(topic) => serializer.serialize_element(&Utf8String(topic))?,
            Property::CorrelationData(data) => serializer.serialize_element(&BinaryData(data))?,
            Property::SubscriptionIdentifier(data) => {
                serializer.serialize_element(&Varint(*data))?
            }
            Property::SessionExpiryInterval(data) => serializer.serialize_element(data)?,
            Property::AssignedClientIdentifier(data) => {
                serializer.serialize_element(&Utf8String(data))?;
            }
            Property::ServerKeepAlive(data) => serializer.serialize_element(data)?,
            Property::AuthenticationMethod(data) => {
                serializer.serialize_element(&Utf8String(data))?
            }
            Property::AuthenticationData(data) => {
                serializer.serialize_element(&BinaryData(data))?
            }
            Property::RequestProblemInformation(data) => serializer.serialize_element(data)?,
            Property::WillDelayInterval(data) => serializer.serialize_element(data)?,
            Property::RequestResponseInformation(data) => serializer.serialize_element(data)?,
            Property::ResponseInformation(data) => {
                serializer.serialize_element(&Utf8String(data))?
            }
            Property::ServerReference(data) => serializer.serialize_element(&Utf8String(data))?,
            Property::ReasonString(data) => serializer.serialize_element(&Utf8String(data))?,
            Property::ReceiveMaximum(data) => serializer.serialize_element(data)?,
            Property::TopicAliasMaximum(data) => serializer.serialize_element(data)?,
            Property::TopicAlias(data) => serializer.serialize_element(data)?,
            Property::MaximumQoS(data) => serializer.serialize_element(data)?,
            Property::RetainAvailable(data) => serializer.serialize_element(data)?,
            Property::UserProperty(key, value) => {
                serializer.serialize_element(&Utf8String(key))?;
                serializer.serialize_element(&Utf8String(value))?;
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

impl Property<'_> {
    pub(crate) fn size(&self) -> usize {
        let identifier: PropertyIdentifier = self.into();
        let identifier_length = Varint(identifier as u32).encoded_len();

        match self {
            Property::ContentType(data)
            | Property::ResponseTopic(data)
            | Property::AuthenticationMethod(data)
            | Property::ResponseInformation(data)
            | Property::ServerReference(data)
            | Property::ReasonString(data)
            | Property::AssignedClientIdentifier(data) => data.len() + 2 + identifier_length,
            Property::UserProperty(key, value) => {
                (value.len() + 2) + (key.len() + 2) + identifier_length
            }
            Property::CorrelationData(data) | Property::AuthenticationData(data) => {
                data.len() + 2 + identifier_length
            }
            Property::SubscriptionIdentifier(id) => Varint(*id).encoded_len() + identifier_length,

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

#[cfg(test)]
mod tests {
    use super::*;
    use heapless::Vec;

    #[test]
    fn iterate_slice_properties() {
        let props = [Property::ReceiveMaximum(2), Property::MaximumQoS(1)];
        let properties = Properties::from_slice(&props);
        let values: Vec<_, 4> = properties.iter().collect();
        let expected: Vec<_, 4> =
            Vec::from_slice(&[Ok(props[0].clone()), Ok(props[1].clone())]).unwrap();
        assert_eq!(values, expected);
    }

    #[test]
    fn iterate_correlated_properties() {
        let props = [Property::ReceiveMaximum(2)];
        let correlation = Property::CorrelationData(b"abc");
        let properties = Properties::from_slice(&props).with_correlation(b"abc");
        let values: Vec<_, 4> = properties.iter().collect();
        let expected: Vec<_, 4> =
            Vec::from_slice(&[Ok(correlation), Ok(props[0].clone())]).unwrap();
        assert_eq!(values, expected);
    }
}
