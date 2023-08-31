use crate::{
    properties::{Property, PropertyIdentifier},
    types::{BinaryData, Properties, Utf8String},
    varint::Varint,
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

    /// Serialize the will contents into a flattened, borrowed buffer.
    pub(crate) fn serialize<'b>(
        &self,
        buf: &'b mut [u8],
    ) -> Result<SerializedWill<'b>, crate::ser::Error> {
        let message = WillMessage {
            topic: Utf8String(self.topic),
            properties: Properties::Slice(self.properties),
            data: BinaryData(self.data),
        };

        let mut serializer = crate::ser::MqttSerializer::new_without_header(buf);
        message.serialize(&mut serializer)?;
        Ok(SerializedWill {
            qos: self.qos,
            retained: self.retained,
            contents: serializer.finish(),
        })
    }

    /// Precalculate the length of the serialized will.
    pub(crate) fn serialized_len(&self) -> usize {
        let prop_len = {
            let prop_size = Properties::Slice(self.properties).size();
            Varint(prop_size as u32).len() + prop_size
        };
        let topic_len = self.topic.len() + core::mem::size_of::<u16>();
        let payload_len = self.data.len() + core::mem::size_of::<u16>();
        topic_len + payload_len + prop_len
    }

    /// Specify the will as a retained message.
    pub fn set_retained(mut self) -> Self {
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
}

/// A will where the topic, properties, and contents have already been serialized.
#[derive(Debug, Copy, Clone, PartialEq)]
pub(crate) struct SerializedWill<'a> {
    pub(crate) qos: QoS,
    pub(crate) retained: Retain,
    pub(crate) contents: &'a [u8],
}
