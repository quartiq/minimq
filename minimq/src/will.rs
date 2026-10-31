use crate::{
    ConfigError, QoS, Retain,
    properties::{Properties, Property, PropertyContext},
    wire::{BinaryData, Utf8String},
};
use heapless::String;

const TOPIC_CAPACITY: usize = 128;
type TopicString = String<TOPIC_CAPACITY>;

/// MQTT will message.
#[derive(Debug, Clone, PartialEq)]
#[must_use]
pub struct Will<'a> {
    pub(crate) topic: TopicString,
    pub(crate) data: &'a [u8],
    qos: QoS,
    retained: Retain,
    pub(crate) properties: &'a [Property<'a>],
}

impl<'a> Will<'a> {
    /// Construct a will.
    ///
    /// Only MQTT v5 properties valid on will messages are accepted.
    pub fn new(
        topic: &str,
        data: &'a [u8],
        properties: &'a [Property<'a>],
    ) -> Result<Self, ConfigError> {
        if !crate::wire::valid_topic(topic)
            || !Properties::from_slice(properties).valid_for(PropertyContext::Will)
        {
            return Err(ConfigError::InvalidConfig);
        }
        let topic = topic.try_into().map_err(|_| ConfigError::TopicTooLong)?;

        Ok(Self {
            topic,
            data,
            properties,
            qos: QoS::AtMostOnce,
            retained: Retain::NotRetained,
        })
    }

    /// Mark the will as retained.
    pub fn retained(mut self) -> Self {
        self.retained = Retain::Retained;
        self
    }

    /// Set the will QoS.
    pub fn qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    pub(crate) fn retained_flag(&self) -> Retain {
        self.retained
    }

    pub(crate) fn qos_level(&self) -> QoS {
        self.qos
    }
}

impl serde::Serialize for Will<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut item = serializer.serialize_struct("Will", 0)?;
        item.serialize_field("properties", &Properties::from_slice(self.properties))?;
        item.serialize_field("topic", &Utf8String(self.topic.as_str()))?;
        item.serialize_field("data", &BinaryData(self.data))?;
        item.end()
    }
}
