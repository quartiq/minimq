use crate::{
    properties::{Property, PropertyIdentifier},
    types::{BinaryData, Properties, Utf8String},
    ProtocolError, QoS, Retain,
};

use serde::Serialize;

#[derive(Debug)]
pub struct Will<'a> {
    topic: &'a str,
    data: &'a [u8],
    qos: QoS,
    retained: Retain,
    properties: &'a [Property<'a>],
}

#[derive(Serialize)]
struct WillMessage<'a> {
    properties: Properties<'a>,
    topic: Utf8String<'a>,
    data: BinaryData<'a>,
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
            topic,
            data,
            properties,
            qos: QoS::AtMostOnce,
            retained: Retain::NotRetained,
        })
    }

    /// Serialize the will contents into a flattened, owned buffer.
    pub(crate) fn serialize<'b>(
        &self,
        buf: &'b mut [u8],
    ) -> Result<SerializedWill<'b>, crate::ser::Error> {
        let message = WillMessage {
            topic: Utf8String(self.topic),
            properties: Properties::Slice(self.properties),
            data: BinaryData(self.data),
        };

        let mut serializer = crate::ser::MqttSerializer::new(buf);
        message.serialize(&mut serializer)?;
        Ok(SerializedWill {
            qos: self.qos,
            retained: self.retained,
            contents: serializer.finish(),
        })
    }

    /// Set the retained status of the will.
    ///
    /// # Args
    /// * `retained` - Specifies the retained state of the will.
    pub fn retained(&mut self, retained: Retain) {
        self.retained = retained;
    }

    /// Set the quality of service at which the will message is sent.
    ///
    /// # Args
    /// * `qos` - The desired quality-of-service level to send the message at.
    pub fn qos(&mut self, qos: QoS) {
        self.qos = qos;
    }
}

/// A will where the topic, properties, and contents have already been serialized.
#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) struct SerializedWill<'a> {
    pub(crate) qos: QoS,
    pub(crate) retained: Retain,
    pub(crate) contents: &'a [u8],
}
