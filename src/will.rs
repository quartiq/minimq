use crate::{
    ProtocolError, QoS, Retain,
    properties::{Property, PropertyIdentifier},
    types::{BinaryData, Properties, Utf8String},
};
use heapless::String;

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

#[derive(Debug, Clone, PartialEq)]
pub struct Will<'a> {
    topic: Topic<'a>,
    data: &'a [u8],
    qos: QoS,
    retained: Retain,
    properties: &'a [Property<'a>],
}

impl<'a> Will<'a> {
    /// Construct a new will message.
    ///
    /// # Args
    /// * `topic` - The topic to send the message on
    /// * `data` - The message to transmit
    /// * `properties` - Any properties to send with the will message.
    pub fn new(
        topic: &'a str,
        data: &'a [u8],
        properties: &'a [Property<'a>],
    ) -> Result<Self, ProtocolError> {
        // Check that the input properties are valid for a will.
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

        Ok(Self {
            topic: Topic::Borrowed(topic),
            data,
            properties,
            qos: QoS::AtMostOnce,
            retained: Retain::NotRetained,
        })
    }

    /// Construct a new will message with an owned topic.
    pub fn new_owned(
        topic: &str,
        data: &'a [u8],
        properties: &'a [Property<'a>],
    ) -> Result<Self, ProtocolError> {
        let topic = String::try_from(topic).map_err(|_| ProtocolError::BufferSize)?;
        Self::new("", data, properties).map(|mut will| {
            will.topic = Topic::Owned(topic);
            will
        })
    }

    /// Specify the will as a retained message.
    pub fn retained(mut self) -> Self {
        self.retained = Retain::Retained;
        self
    }

    /// Set the quality of service at which the will message is sent.
    ///
    /// # Args
    /// * `qos` - The desired quality-of-service level to send the message at.
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
