use crate::{
    properties::{Property, PropertyIdentifier},
    ser::serializer::MqttSerializer,
    types::{BinaryData, Properties, Utf8String},
    ProtocolError, QoS, Retain,
};

use serde::Serialize;

use heapless::Vec;

#[derive(Serialize, Debug)]
pub struct Will<const MSG_SIZE: usize> {
    pub(crate) payload: Vec<u8, MSG_SIZE>,
    #[serde(skip)]
    pub(crate) qos: QoS,
    #[serde(skip)]
    pub(crate) retain: Retain,
}

#[derive(Serialize)]
struct WillMessage<'a> {
    properties: Properties<'a>,
    topic: Utf8String<'a>,
    data: BinaryData<'a>,
}

impl<const MSG_SIZE: usize> Will<MSG_SIZE> {
    /// Construct a new will message.
    ///
    /// # Args
    /// * `topic` - The topic to send the message on
    /// * `data` - The message to transmit
    /// * `properties` - Any properties to send with the will message.
    pub fn new(topic: &str, data: &[u8], properties: &[Property]) -> Result<Self, ProtocolError> {
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

        let will_data = WillMessage {
            topic: Utf8String(topic),
            properties: Properties(properties),
            data: BinaryData(data),
        };

        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let mut serializer = MqttSerializer::new(&mut buffer);
        will_data.serialize(&mut serializer)?;

        Ok(Self {
            qos: QoS::AtMostOnce,
            retain: Retain::NotRetained,
            // Note(unwrap): The vectro is declared as identical size to the vector, so it will
            // always fit.
            payload: Vec::from_slice(serializer.finish()).unwrap(),
        })
    }

    /// Set the retained status of the will.
    ///
    /// # Args
    /// * `retained` - Specifies the retained state of the will.
    pub fn retained(&mut self, retained: Retain) {
        self.retain = retained;
    }

    /// Set the quality of service at which the will message is sent.
    ///
    /// # Args
    /// * `qos` - The desired quality-of-service level to send the message at.
    pub fn qos(&mut self, qos: QoS) {
        self.qos = qos;
    }
}
