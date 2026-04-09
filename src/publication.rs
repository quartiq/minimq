use crate::properties::Property;
use crate::types::{BinaryData, Properties};
use crate::{ProtocolError, QoS, Retain};
use heapless::{String, Vec};

pub trait ToPayload {
    type Error;
    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error>;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct ResponseTarget<'a> {
    pub(crate) topic: &'a str,
    pub(crate) correlation_data: Option<&'a [u8]>,
}

impl<'a> ResponseTarget<'a> {
    pub fn topic(&self) -> &'a str {
        self.topic
    }

    pub fn correlation_data(&self) -> Option<&'a [u8]> {
        self.correlation_data
    }

    pub fn publication<P>(self, payload: P) -> Publication<'a, P> {
        let publication = Publication::new(self.topic, payload);
        match self.correlation_data {
            Some(data) => publication.correlate(data),
            None => publication,
        }
    }

    pub fn to_owned<const TOPIC: usize, const CORRELATION: usize>(
        self,
    ) -> Result<OwnedResponseTarget<TOPIC, CORRELATION>, ProtocolError> {
        Ok(OwnedResponseTarget {
            topic: String::try_from(self.topic).map_err(|_| ProtocolError::BufferSize)?,
            correlation_data: self
                .correlation_data
                .map(Vec::try_from)
                .transpose()
                .map_err(|_| ProtocolError::BufferSize)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedResponseTarget<const TOPIC: usize, const CORRELATION: usize> {
    topic: String<TOPIC>,
    correlation_data: Option<Vec<u8, CORRELATION>>,
}

impl<const TOPIC: usize, const CORRELATION: usize> OwnedResponseTarget<TOPIC, CORRELATION> {
    pub fn topic(&self) -> &str {
        self.topic.as_str()
    }

    pub fn correlation_data(&self) -> Option<&[u8]> {
        self.correlation_data.as_deref()
    }

    pub fn publication<'a, P>(&'a self, payload: P) -> Publication<'a, P> {
        let publication = Publication::new(self.topic.as_str(), payload);
        match self.correlation_data.as_deref() {
            Some(data) => publication.correlate(data),
            None => publication,
        }
    }
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

pub struct Publication<'a, P> {
    pub(crate) topic: &'a str,
    pub(crate) properties: Properties<'a>,
    pub(crate) qos: QoS,
    pub(crate) payload: P,
    pub(crate) retain: Retain,
}

impl<'a, P> Publication<'a, P> {
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

    pub fn new(topic: &'a str, payload: P) -> Self {
        Self {
            payload,
            qos: QoS::AtMostOnce,
            topic,
            properties: Properties::Slice(&[]),
            retain: Retain::NotRetained,
        }
    }

    pub fn topic(&self) -> &'a str {
        self.topic
    }

    pub fn properties_ref(&self) -> &Properties<'a> {
        &self.properties
    }

    pub fn qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    pub fn retain(mut self) -> Self {
        self.retain = Retain::Retained;
        self
    }

    pub fn properties(mut self, properties: &'a [Property<'a>]) -> Self {
        self.properties = match self.properties {
            Properties::Slice(_) => Properties::Slice(properties),
            Properties::CorrelatedSlice { correlation, .. } => Properties::CorrelatedSlice {
                correlation,
                properties,
            },
            Properties::DataBlock(_) => Properties::Slice(properties),
        };
        self
    }

    pub fn correlate(mut self, data: &'a [u8]) -> Self {
        self.properties = match self.properties {
            Properties::Slice(properties) | Properties::CorrelatedSlice { properties, .. } => {
                Properties::CorrelatedSlice {
                    properties,
                    correlation: Property::CorrelationData(BinaryData(data)),
                }
            }
            Properties::DataBlock(_) => Properties::CorrelatedSlice {
                properties: &[],
                correlation: Property::CorrelationData(BinaryData(data)),
            },
        };

        self
    }
}
