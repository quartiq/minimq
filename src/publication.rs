use crate::{
    packets::Pub,
    properties::Property,
    types::{BinaryData, Properties, Utf8String},
    ProtocolError, QoS, Retain,
};

pub trait ToPayload {
    type Error;
    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error>;
}

impl<'a> ToPayload for &'a [u8] {
    type Error = ();

    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        if buffer.len() < self.len() {
            return Err(());
        }
        buffer[..self.len()].copy_from_slice(self);
        Ok(self.len())
    }
}

impl<'a> ToPayload for &'a str {
    type Error = ();

    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error> {
        self.as_bytes().serialize(buffer)
    }
}

impl<const N: usize> ToPayload for [u8; N] {
    type Error = ();

    fn serialize(self, buffer: &mut [u8]) -> Result<usize, ()> {
        (&self[..]).serialize(buffer)
    }
}
impl<const N: usize> ToPayload for &[u8; N] {
    type Error = ();

    fn serialize(self, buffer: &mut [u8]) -> Result<usize, ()> {
        (&self[..]).serialize(buffer)
    }
}

pub struct DeferredPublication<E, F: FnOnce(&mut [u8]) -> Result<usize, E>> {
    func: F,
}

impl<E, F: FnOnce(&mut [u8]) -> Result<usize, E>> DeferredPublication<E, F> {
    pub fn new<'a>(func: F) -> Publication<'a, Self> {
        Publication::new(Self { func })
    }
}

impl<E, F: FnOnce(&mut [u8]) -> Result<usize, E>> ToPayload for DeferredPublication<E, F> {
    type Error = E;
    fn serialize(self, buffer: &mut [u8]) -> Result<usize, E> {
        (self.func)(buffer)
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
pub struct Publication<'a, P: ToPayload> {
    topic: Option<&'a str>,
    properties: Properties<'a>,
    qos: QoS,
    payload: P,
    retain: Retain,
}

impl<'a, P: ToPayload> Publication<'a, P> {
    /// Construct a new publication with a payload.
    pub fn new(payload: P) -> Self {
        Self {
            payload,
            qos: QoS::AtMostOnce,
            topic: None,
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

    /// Specify the publication topic for this message.
    ///
    /// # Note
    /// If this is called after [Publication::reply] determines a response topic, the response
    /// topic will be overridden.
    pub fn topic(mut self, topic: &'a str) -> Self {
        self.topic.replace(topic);
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

    /// Generate the publication as a reply to some other received message.
    ///
    /// # Note
    /// The received message properties are parsed for both [Property::CorrelationData] and
    /// [PropertyResponseTopic].
    ///
    /// * If correlation data is found, it is automatically appended to the
    /// publication properties.
    ///
    /// * If a response topic is identified, the message topic will be
    /// configured for it, which will override any previously-specified topic.
    pub fn reply(mut self, received_properties: &'a Properties<'a>) -> Self {
        if let Some(response_topic) = received_properties.into_iter().find_map(|p| {
            if let Ok(Property::ResponseTopic(topic)) = p {
                Some(topic.0)
            } else {
                None
            }
        }) {
            self.topic.replace(response_topic);
        }

        // Next, copy over any correlation data to the outbound properties.
        if let Some(correlation_data) = received_properties.into_iter().find_map(|p| {
            if let Ok(Property::CorrelationData(data)) = p {
                Some(data.0)
            } else {
                None
            }
        }) {
            self.correlate(correlation_data)
        } else {
            self
        }
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

    /// Generate the final publication.
    ///
    /// # Returns
    /// The message to be published if a publication topic was specified. If no publication topic
    /// was identified, an error is returned.
    pub fn finish(self) -> Result<Pub<'a, P>, ProtocolError> {
        Ok(Pub {
            topic: Utf8String(self.topic.ok_or(ProtocolError::NoTopic)?),
            properties: self.properties,
            packet_id: None,
            payload: self.payload,
            retain: self.retain,
            qos: self.qos,
            dup: false,
        })
    }
}
