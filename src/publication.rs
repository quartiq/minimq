use crate::{
    properties::Property,
    types::{BinaryData, Properties},
    ProtocolError, QoS, Retain,
};

pub trait ToPayload {
    type Error;
    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error>;
}

impl ToPayload for &[u8] {
    type Error = ();

    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        if buffer.len() < self.len() {
            return Err(());
        }
        buffer[..self.len()].copy_from_slice(self);
        Ok(self.len())
    }
}

impl ToPayload for &str {
    type Error = ();

    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.as_bytes().serialize(buffer)
    }
}

impl<const N: usize> ToPayload for &[u8; N] {
    type Error = ();

    fn serialize(self, buffer: &mut [u8]) -> Result<usize, ()> {
        (&self[..]).serialize(buffer)
    }
}

impl<E, F: FnOnce(&mut [u8]) -> Result<usize, E>> ToPayload for F {
    type Error = E;
    fn serialize(self, buffer: &mut [u8]) -> Result<usize, E> {
        self(buffer)
    }
}

/// Builder pattern for generating MQTT publications.
///
/// # Note
/// By default, messages are constructed with:
/// * A QoS setting of [QoS::AtMostOnce]
/// * No properties
/// * No destination topic
/// * Retention set to [Retain::NotRetained]
///
/// It is expected that the user provide a topic either by directly specifying a publication topic
/// in [Publication::topic], or by parsing a topic from the [Property::ResponseTopic] property
/// contained within received properties by using the [Publication::reply] API.
pub struct Publication<'a, P> {
    pub(crate) topic: &'a str,
    pub(crate) properties: Properties<'a>,
    pub(crate) qos: QoS,
    pub(crate) payload: P,
    pub(crate) retain: Retain,
}

impl<'a, P> Publication<'a, P> {
    /// Generate the publication as a reply to some other received message.
    ///
    /// # Note
    /// The received message properties are parsed for both [Property::CorrelationData] and
    /// [Property::ResponseTopic].
    ///
    /// * If correlation data is found, it is automatically appended to the
    ///   publication properties.
    ///
    /// * If a response topic is identified, the message topic will be
    ///   configured for it, which will override the default topic.
    pub fn respond(
        default_topic: Option<&'a str>,
        received_properties: &'a Properties<'a>,
        payload: P,
    ) -> Result<Self, ProtocolError> {
        let response_topic = received_properties
            .into_iter()
            .flatten()
            .find_map(|p| {
                if let Property::ResponseTopic(topic) = p {
                    Some(topic.0)
                } else {
                    None
                }
            })
            .or(default_topic)
            .ok_or(ProtocolError::NoTopic)?;

        let publication = Self::new(response_topic, payload);

        for p in received_properties.into_iter().flatten() {
            if let Property::CorrelationData(data) = p {
                return Ok(publication.correlate(data.0));
            }
        }
        Ok(publication)
    }

    /// Construct a new publication with a payload.
    pub fn new(topic: &'a str, payload: P) -> Self {
        Self {
            payload,
            qos: QoS::AtMostOnce,
            topic,
            properties: Properties::Slice(&[]),
            retain: Retain::NotRetained,
        }
    }

    /// Specify the [QoS] of the publication. By default, the QoS is set to [QoS::AtMostOnce].
    pub fn qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    /// Specify that this message should be [Retain::Retained].
    pub fn retain(mut self) -> Self {
        self.retain = Retain::Retained;
        self
    }

    /// Specify properties associated with this publication.
    pub fn properties(mut self, properties: &'a [Property<'a>]) -> Self {
        self.properties = match self.properties {
            Properties::Slice(_) => Properties::Slice(properties),
            Properties::CorrelatedSlice { correlation, .. } => Properties::CorrelatedSlice {
                correlation,
                properties,
            },
            _ => unimplemented!(),
        };
        self
    }

    /// Include correlation data to the message
    ///
    /// # Note
    /// This will override any existing correlation data in the message.
    ///
    /// # Args
    /// * `data` - The data composing the correlation data.
    pub fn correlate(mut self, data: &'a [u8]) -> Self {
        self.properties = match self.properties {
            Properties::Slice(properties) | Properties::CorrelatedSlice { properties, .. } => {
                Properties::CorrelatedSlice {
                    properties,
                    correlation: Property::CorrelationData(BinaryData(data)),
                }
            }
            _ => unimplemented!(),
        };

        self
    }
}
