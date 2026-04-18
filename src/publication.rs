use crate::properties::Property;
use crate::types::Properties;
use crate::{ProtocolError, QoS, Retain};
use heapless::{String, Vec};

/// Trait for values that can serialize themselves into a publish payload buffer.
///
/// The implementation writes payload bytes into the provided buffer and returns the payload length.
pub trait ToPayload {
    /// Payload serialization error.
    type Error;
    /// Serialize the payload into `buffer`.
    fn serialize(self, buffer: &mut [u8]) -> Result<usize, Self::Error>;
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) struct ResponseTarget<'a> {
    pub(crate) topic: &'a str,
    pub(crate) correlation_data: Option<&'a [u8]>,
}

impl<'a> ResponseTarget<'a> {
    pub(crate) fn publication<P>(self, payload: P) -> Publication<'a, P> {
        let mut publication = Publication::new(self.topic, payload);
        if let Some(data) = self.correlation_data {
            publication = publication.correlate(data);
        }
        publication
    }

    pub(crate) fn to_owned<const TOPIC: usize, const CORRELATION: usize>(
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
/// Owned MQTT request/reply target captured from an inbound publish.
pub struct OwnedResponseTarget<const TOPIC: usize, const CORRELATION: usize> {
    topic: String<TOPIC>,
    correlation_data: Option<Vec<u8, CORRELATION>>,
}

impl<const TOPIC: usize, const CORRELATION: usize> OwnedResponseTarget<TOPIC, CORRELATION> {
    /// Return the response topic.
    pub fn topic(&self) -> &str {
        self.topic.as_str()
    }

    /// Return the response correlation data, if present.
    pub fn correlation_data(&self) -> Option<&[u8]> {
        self.correlation_data.as_deref()
    }

    /// Build a publication addressed to this response target.
    pub fn publication<'a, P>(&'a self, payload: P) -> Publication<'a, P> {
        let mut publication = Publication::new(self.topic.as_str(), payload);
        if let Some(data) = self.correlation_data.as_deref() {
            publication = publication.correlate(data);
        }
        publication
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

/// Builder for an outbound MQTT `PUBLISH`.
pub struct Publication<'a, P> {
    pub(crate) topic: &'a str,
    pub(crate) properties: Properties<'a>,
    pub(crate) qos: QoS,
    pub(crate) payload: P,
    pub(crate) retain: Retain,
}

impl<'a, P> Publication<'a, P> {
    /// Construct a publication with QoS 0, no retain flag, and no user properties.
    pub fn new(topic: &'a str, payload: P) -> Self {
        Self {
            payload,
            qos: QoS::AtMostOnce,
            topic,
            properties: Properties::Slice(&[]),
            retain: Retain::NotRetained,
        }
    }

    /// Return the current MQTT v5 property set for this publish.
    pub fn properties_ref(&self) -> &Properties<'a> {
        &self.properties
    }

    /// Set the requested publish QoS.
    pub fn qos(mut self, qos: QoS) -> Self {
        self.qos = qos;
        self
    }

    /// Mark the publication as retained.
    pub fn retain(mut self) -> Self {
        self.retain = Retain::Retained;
        self
    }

    /// Attach MQTT v5 publish properties.
    pub fn properties(mut self, properties: &'a [Property<'a>]) -> Self {
        self.properties = self.properties.with_properties(properties);
        self
    }

    /// Attach MQTT v5 correlation data.
    pub fn correlate(mut self, data: &'a [u8]) -> Self {
        self.properties = self.properties.with_correlation(data);
        self
    }
}
