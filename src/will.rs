use crate::{
    ProtocolError, QoS, Retain,
    properties::{Property, PropertyIdentifier},
    types::{BinaryData, Properties, Utf8String},
};
use heapless::String;

fn validate_will_properties(properties: &[Property<'_>]) -> Result<(), ProtocolError> {
    for property in properties {
        match property.into() {
            PropertyIdentifier::WillDelayInterval
            | PropertyIdentifier::PayloadFormatIndicator
            | PropertyIdentifier::MessageExpiryInterval
            | PropertyIdentifier::ContentType
            | PropertyIdentifier::ResponseTopic
            | PropertyIdentifier::CorrelationData
            | PropertyIdentifier::UserProperty => {}
            _ => return Err(ProtocolError::InvalidProperty),
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq)]
enum Topic<'a> {
    Borrowed(&'a str),
    Owned(String<128>),
}

impl Topic<'_> {
    fn as_str(&self) -> &str {
        match self {
            Self::Borrowed(topic) => topic,
            Self::Owned(topic) => topic.as_str(),
        }
    }
}

/// MQTT will message.
#[derive(Debug, Clone, PartialEq)]
pub struct Will<'a> {
    topic: Topic<'a>,
    data: &'a [u8],
    qos: QoS,
    retained: Retain,
    properties: &'a [Property<'a>],
}

impl<'a> Will<'a> {
    /// Construct a will that borrows its topic.
    ///
    /// Only MQTT v5 properties valid on will messages are accepted.
    pub fn new(
        topic: &'a str,
        data: &'a [u8],
        properties: &'a [Property<'a>],
    ) -> Result<Self, ProtocolError> {
        validate_will_properties(properties)?;

        Ok(Self {
            topic: Topic::Borrowed(topic),
            data,
            properties,
            qos: QoS::AtMostOnce,
            retained: Retain::NotRetained,
        })
    }

    /// Construct a will that owns its fixed-capacity topic.
    pub fn owned(
        topic: &str,
        data: &'a [u8],
        properties: &'a [Property<'a>],
    ) -> Result<Self, ProtocolError> {
        validate_will_properties(properties)?;
        Ok(Self {
            topic: Topic::Owned(String::try_from(topic).map_err(|_| ProtocolError::BufferSize)?),
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
        item.serialize_field("properties", &Properties::Slice(self.properties))?;
        item.serialize_field("topic", &Utf8String(self.topic.as_str()))?;
        item.serialize_field("data", &BinaryData(self.data))?;
        item.end()
    }
}
