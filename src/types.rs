//! MQTT-specific data types used by the public API.
use crate::{
    ProtocolError, QoS, de::deserializer::MqttDeserializer, properties::Property, varint::Varint,
};
use bit_field::BitField;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

/// MQTT property collection attached to a packet.
///
/// Application code usually receives this from inbound packets or passes a borrowed slice when
/// publishing or subscribing.
#[derive(Debug, PartialEq)]
pub enum Properties<'a> {
    /// Borrowed property slice.
    Slice(&'a [Property<'a>]),
    /// Borrowed encoded property block received from the broker.
    DataBlock(&'a [u8]),
    /// Borrowed property slice with synthetic correlation data prepended.
    CorrelatedSlice {
        /// Correlation data to expose first.
        correlation: Property<'a>,
        /// Remaining borrowed properties.
        properties: &'a [Property<'a>],
    },
}

impl Properties<'_> {
    /// Return the encoded MQTT property block size in bytes.
    pub fn size(&self) -> usize {
        match self {
            Properties::Slice(props) => props.iter().map(|prop| prop.size()).sum(),
            Properties::CorrelatedSlice {
                correlation,
                properties,
            } => properties
                .iter()
                .chain([correlation.clone()].iter())
                .map(|prop| prop.size())
                .sum(),
            Properties::DataBlock(block) => block.len(),
        }
    }
}

impl<'a> Properties<'a> {
    /// Iterate over properties.
    pub fn iter(&'a self) -> PropertiesIter<'a> {
        self.into_iter()
    }

    /// Return the first `ResponseTopic` property, if present.
    pub fn response_topic(&'a self) -> Option<&'a str> {
        self.iter().response_topic()
    }

    /// Return the first `CorrelationData` property, if present.
    pub fn correlation_data(&'a self) -> Option<&'a [u8]> {
        self.iter().correlation_data()
    }

    pub(crate) fn with_properties(self, properties: &'a [Property<'a>]) -> Self {
        match self {
            Self::CorrelatedSlice { correlation, .. } => Self::CorrelatedSlice {
                correlation,
                properties,
            },
            Self::Slice(_) | Self::DataBlock(_) => Self::Slice(properties),
        }
    }

    pub(crate) fn with_correlation(self, data: &'a [u8]) -> Self {
        let correlation = Property::CorrelationData(BinaryData(data));
        match self {
            Self::Slice(properties) | Self::CorrelatedSlice { properties, .. } => {
                Self::CorrelatedSlice {
                    correlation,
                    properties,
                }
            }
            Self::DataBlock(_) => Self::CorrelatedSlice {
                correlation,
                properties: &[],
            },
        }
    }
}

/// Iterator over decoded MQTT properties.
pub struct PropertiesIter<'a> {
    inner: PropertiesIterInner<'a>,
}

enum PropertiesIterInner<'a> {
    DataBlock {
        props: &'a [u8],
        index: usize,
    },
    Slice {
        props: &'a [Property<'a>],
        index: usize,
    },
    Correlated {
        correlation: Property<'a>,
        yielded_correlation: bool,
        props: &'a [Property<'a>],
        index: usize,
    },
}

impl<'a> PropertiesIter<'a> {
    /// Return the first `ResponseTopic` property in the remaining iterator.
    pub fn response_topic(&mut self) -> Option<&'a str> {
        self.find_map(|prop| match prop {
            Ok(crate::Property::ResponseTopic(topic)) => Some(topic.0),
            _ => None,
        })
    }

    /// Return the first `CorrelationData` property in the remaining iterator.
    pub fn correlation_data(&mut self) -> Option<&'a [u8]> {
        self.find_map(|prop| match prop {
            Ok(crate::Property::CorrelationData(data)) => Some(data.0),
            _ => None,
        })
    }
}

impl<'a> core::iter::Iterator for PropertiesIter<'a> {
    type Item = Result<Property<'a>, ProtocolError>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.inner {
            PropertiesIterInner::DataBlock { props, index } => {
                if *index >= props.len() {
                    return None;
                }

                let mut deserializer = MqttDeserializer::new(&props[*index..]);
                let property = Property::deserialize(&mut deserializer)
                    .map_err(ProtocolError::Deserialization);
                *index += deserializer.deserialized_bytes();
                Some(property)
            }
            PropertiesIterInner::Slice { props, index } => {
                let property = props.get(*index).cloned()?;
                *index += 1;
                Some(Ok(property))
            }
            PropertiesIterInner::Correlated {
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

impl<'a> core::iter::IntoIterator for &'a Properties<'a> {
    type Item = Result<Property<'a>, ProtocolError>;
    type IntoIter = PropertiesIter<'a>;

    fn into_iter(self) -> PropertiesIter<'a> {
        match self {
            Properties::DataBlock(data) => PropertiesIter {
                inner: PropertiesIterInner::DataBlock {
                    props: data,
                    index: 0,
                },
            },
            Properties::Slice(props) => PropertiesIter {
                inner: PropertiesIterInner::Slice { props, index: 0 },
            },
            Properties::CorrelatedSlice {
                correlation,
                properties,
            } => PropertiesIter {
                inner: PropertiesIterInner::Correlated {
                    correlation: correlation.clone(),
                    yielded_correlation: false,
                    props: properties,
                    index: 0,
                },
            },
        }
    }
}

impl serde::Serialize for Properties<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut item = serializer.serialize_struct("Properties", 0)?;
        item.serialize_field("_len", &Varint(self.size() as u32))?;

        match self {
            Properties::Slice(props) => {
                item.serialize_field("_props", props)?;
            }
            Properties::CorrelatedSlice {
                correlation,
                properties,
            } => {
                item.serialize_field("_correlation", &correlation)?;
                item.serialize_field("_props", properties)?;
            }
            Properties::DataBlock(block) => {
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
        Ok(Properties::DataBlock(data.unwrap_or(&[])))
    }
}

impl<'a, 'de: 'a> serde::de::Deserialize<'de> for Properties<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_seq(PropertiesVisitor)
    }
}

/// MQTT binary data field.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BinaryData<'a>(pub &'a [u8]);

impl serde::Serialize for BinaryData<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let len = u16::try_from(self.0.len())
            .map_err(|_| S::Error::custom("Provided binary data is too long"))?;
        let mut item = serializer.serialize_struct("_BinaryData", 0)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use heapless::Vec as HVec;

    #[test]
    fn iterate_slice_properties() {
        let props = [Property::ReceiveMaximum(2), Property::MaximumQoS(1)];
        let properties = Properties::Slice(&props);
        let values: HVec<_, 4> = (&properties).into_iter().collect();
        let expected: HVec<_, 4> =
            HVec::from_slice(&[Ok(props[0].clone()), Ok(props[1].clone())]).unwrap();
        assert_eq!(values, expected);
    }

    #[test]
    fn iterate_correlated_properties() {
        let props = [Property::ReceiveMaximum(2)];
        let correlation = Property::CorrelationData(BinaryData(b"abc"));
        let properties = Properties::CorrelatedSlice {
            correlation: correlation.clone(),
            properties: &props,
        };
        let values: HVec<_, 4> = (&properties).into_iter().collect();
        let expected: HVec<_, 4> =
            HVec::from_slice(&[Ok(correlation), Ok(props[0].clone())]).unwrap();
        assert_eq!(values, expected);
    }

    #[test]
    fn default_subscription_options_cap_inbound_qos_to_at_most_once() {
        let filter = TopicFilter::new("demo/in");
        assert_eq!(filter.options.maximum_qos, QoS::AtMostOnce);
    }
}

impl<'de> serde::de::Deserialize<'de> for BinaryData<'de> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(BinaryDataVisitor)
    }
}

/// Username/password authentication data used in `CONNECT`.
#[derive(Debug, Copy, Clone)]
pub struct Auth<'a> {
    /// MQTT username.
    pub user_name: &'a str,
    /// MQTT password bytes.
    pub password: &'a [u8],
}

/// MQTT UTF-8 string field.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Utf8String<'a>(pub &'a str);

impl serde::Serialize for Utf8String<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let len = u16::try_from(self.0.len())
            .map_err(|_| S::Error::custom("Provided string is too long"))?;
        let mut item = serializer.serialize_struct("_Utf8String", 0)?;
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
        deserializer.deserialize_str(Utf8StringVisitor {
            _data: core::marker::PhantomData,
        })
    }
}

/// Broker retain handling policy for a subscription.
#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum RetainHandling {
    /// Send retained messages when the subscription is created.
    Immediately = 0b00,
    /// Send retained messages only if the subscription did not already exist.
    IfSubscriptionDoesNotExist = 0b01,
    /// Never send retained messages because of the subscription.
    Never = 0b10,
}

/// MQTT subscription options for one topic filter.
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
    /// Cap the maximum QoS delivered for this subscription.
    pub fn maximum_qos(mut self, qos: QoS) -> Self {
        self.maximum_qos = qos;
        self
    }

    /// Choose how retained messages are replayed when subscribing.
    pub fn retain_behavior(mut self, handling: RetainHandling) -> Self {
        self.retain_behavior = handling;
        self
    }

    /// Suppress messages published by this same client.
    pub fn ignore_local_messages(mut self) -> Self {
        self.no_local = true;
        self
    }

    /// Preserve the broker's retain flag on forwarded retained messages.
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

/// Topic filter and options for `SUBSCRIBE`.
#[derive(Serialize, Copy, Clone, Debug)]
pub struct TopicFilter<'a> {
    topic: Utf8String<'a>,
    options: SubscriptionOptions,
}

impl<'a> TopicFilter<'a> {
    /// Construct a topic filter with default subscription options.
    ///
    /// ```rust
    /// use minimq::types::{SubscriptionOptions, TopicFilter};
    ///
    /// let filter = TopicFilter::new("demo/in").options(SubscriptionOptions::default());
    /// let _ = filter;
    /// ```
    pub fn new(topic: &'a str) -> Self {
        Self {
            topic: Utf8String(topic),
            options: SubscriptionOptions::default(),
        }
    }

    /// Override the default subscription options.
    pub fn options(mut self, options: SubscriptionOptions) -> Self {
        self.options = options;
        self
    }
}
