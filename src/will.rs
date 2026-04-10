use crate::{
    ProtocolError, QoS, Retain,
    properties::{Property, PropertyIdentifier},
    types::{BinaryData, Properties, Utf8String},
};
use heapless::String;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WillSpec<'a> {
    Borrowed(Will<'a>),
    Owned(OwnedWill<'a>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Will<'a> {
    topic: &'a str,
    data: &'a [u8],
    qos: QoS,
    retained: Retain,
    properties: &'a [Property<'a>],
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnedWill<'a> {
    topic: String<128>,
    data: &'a [u8],
    qos: QoS,
    retained: Retain,
    properties: &'a [Property<'a>],
}

impl<'a> Will<'a> {
    pub fn new(
        topic: &'a str,
        data: &'a [u8],
        properties: &'a [Property<'a>],
    ) -> Result<Self, ProtocolError> {
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
            topic,
            data,
            properties,
            qos: QoS::AtMostOnce,
            retained: Retain::NotRetained,
        })
    }

    pub fn retained(mut self) -> Self {
        self.retained = Retain::Retained;
        self
    }

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

impl<'a> OwnedWill<'a> {
    pub fn new(
        topic: &str,
        data: &'a [u8],
        properties: &'a [Property<'a>],
    ) -> Result<Self, ProtocolError> {
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
            topic: String::try_from(topic).map_err(|_| ProtocolError::BufferSize)?,
            data,
            properties,
            qos: QoS::AtMostOnce,
            retained: Retain::NotRetained,
        })
    }

    pub fn retained(mut self) -> Self {
        self.retained = Retain::Retained;
        self
    }

    pub fn qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }
}

impl<'a> From<&'a OwnedWill<'a>> for Will<'a> {
    fn from(will: &'a OwnedWill<'a>) -> Self {
        Self {
            topic: will.topic.as_str(),
            data: will.data,
            qos: will.qos,
            retained: will.retained,
            properties: will.properties,
        }
    }
}

impl<'a> WillSpec<'a> {
    pub(crate) fn as_will(&'a self) -> Will<'a> {
        match self {
            Self::Borrowed(will) => will.clone(),
            Self::Owned(will) => will.into(),
        }
    }
}

impl serde::Serialize for Will<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;

        let mut item = serializer.serialize_struct("Will", 0)?;
        item.serialize_field("properties", &Properties::Slice(self.properties))?;
        item.serialize_field("topic", &Utf8String(self.topic))?;
        item.serialize_field("data", &BinaryData(self.data))?;
        item.end()
    }
}
