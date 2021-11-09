use crate::{
    properties::{Property, PropertyIdentifier},
    ser::ReversedPacketWriter,
    ProtocolError, QoS,
};

use heapless::Vec;

pub struct Will<const MSG_SIZE: usize> {
    pub payload: Vec<u8, MSG_SIZE>,
    pub qos: QoS,
    pub retain: bool,
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
            match property.id() {
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

        let mut buffer: [u8; MSG_SIZE] = [0; MSG_SIZE];
        let mut packet = ReversedPacketWriter::new(&mut buffer);

        // Serialize the will payload
        packet.write_binary_data(data)?;
        packet.write_utf8_string(topic)?;
        packet.write_properties(properties)?;

        Ok(Self {
            qos: QoS::AtMostOnce,
            retain: false,
            // Note(unwrap): The vectro is declared as identical size to the vector, so it will
            // always fit.
            payload: Vec::from_slice(packet.finish()).unwrap(),
        })
    }

    /// Set the retained status of the will.
    ///
    /// # Args
    /// * `retained` - True if the will message should be retained by the broker.
    pub fn retained(&mut self, retained: bool) {
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
