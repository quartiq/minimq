//! MQTT-specific data types and serde adapters.
use crate::{
    ProtocolError, QoS, de::deserializer::MqttDeserializer, properties::Property, varint::Varint,
};
use bit_field::BitField;
use serde::ser::SerializeStruct;
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq)]
pub enum Properties<'a> {
    Slice(&'a [Property<'a>]),
    DataBlock(&'a [u8]),
    CorrelatedSlice {
        correlation: Property<'a>,
        properties: &'a [Property<'a>],
    },
}

impl Properties<'_> {
    pub fn size(&self) -> usize {
        match self {
            Properties::Slice(props) => props.iter().map(|prop| prop.size()).sum(),
            Properties::CorrelatedSlice {
                correlation,
                properties,
            } => properties
                .iter()
                .chain([*correlation].iter())
                .map(|prop| prop.size())
                .sum(),
            Properties::DataBlock(block) => block.len(),
        }
    }
}

impl<'a> Properties<'a> {
    pub fn iter(&'a self) -> PropertiesIter<'a> {
        self.into_iter()
    }

    pub fn response_topic(&'a self) -> Option<&'a str> {
        self.iter().response_topic()
    }

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
    pub fn response_topic(&mut self) -> Option<&'a str> {
        self.find_map(|prop| match prop {
            Ok(crate::Property::ResponseTopic(topic)) => Some(topic.0),
            _ => None,
        })
    }

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
                let property = props.get(*index).copied()?;
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
                    return Some(Ok(*correlation));
                }

                let property = props.get(*index).copied()?;
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
                    correlation: *correlation,
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
        let expected: HVec<_, 4> = HVec::from_slice(&[Ok(props[0]), Ok(props[1])]).unwrap();
        assert_eq!(values, expected);
    }

    #[test]
    fn iterate_correlated_properties() {
        let props = [Property::ReceiveMaximum(2)];
        let correlation = Property::CorrelationData(BinaryData(b"abc"));
        let properties = Properties::CorrelatedSlice {
            correlation,
            properties: &props,
        };
        let values: HVec<_, 4> = (&properties).into_iter().collect();
        let expected: HVec<_, 4> = HVec::from_slice(&[Ok(correlation), Ok(props[0])]).unwrap();
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

#[derive(Debug, Copy, Clone)]
pub struct Auth<'a> {
    pub user_name: &'a str,
    pub password: &'a [u8],
}

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

#[derive(Copy, Clone, Debug)]
#[repr(u8)]
pub enum RetainHandling {
    Immediately = 0b00,
    IfSubscriptionDoesNotExist = 0b01,
    Never = 0b10,
}

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
    pub fn maximum_qos(mut self, qos: QoS) -> Self {
        self.maximum_qos = qos;
        self
    }

    pub fn retain_behavior(mut self, handling: RetainHandling) -> Self {
        self.retain_behavior = handling;
        self
    }

    pub fn ignore_local_messages(mut self) -> Self {
        self.no_local = true;
        self
    }

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

    pub fn options(mut self, options: SubscriptionOptions) -> Self {
        self.options = options;
        self
    }
}
