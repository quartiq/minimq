mod core;
mod outbound;
mod protocol;
mod session;

pub use session::Session;

use crate::{
    ProtocolError, QoS, Retain,
    publication::{OwnedResponseTarget, Publication, ResponseTarget},
    types::Properties,
};
use embedded_io_async::{ErrorType, Read, ReadReady, Write, WriteReady};

/// Transport trait required by [`Session`](crate::Session).
pub trait Io: Read + Write + ReadReady + WriteReady + ErrorType {}

impl<T> Io for T where T: Read + Write + ReadReady + WriteReady + ErrorType {}

/// Inbound MQTT `PUBLISH` delivered by [`Event::Inbound`].
#[derive(Debug)]
pub struct InboundPublish<'a> {
    topic: &'a str,
    payload: &'a [u8],
    properties: Properties<'a>,
    retain: Retain,
    qos: QoS,
}

impl<'a> InboundPublish<'a> {
    pub(crate) fn new(
        topic: &'a str,
        payload: &'a [u8],
        properties: Properties<'a>,
        retain: Retain,
        qos: QoS,
    ) -> Self {
        Self {
            topic,
            payload,
            properties,
            retain,
            qos,
        }
    }

    /// Return the inbound topic name.
    pub const fn topic(&self) -> &'a str {
        self.topic
    }

    /// Return the inbound payload bytes.
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Return the inbound MQTT v5 properties.
    pub const fn properties(&self) -> &Properties<'a> {
        &self.properties
    }

    /// Return the inbound retain flag.
    pub const fn retain(&self) -> Retain {
        self.retain
    }

    /// Return the inbound QoS level.
    pub const fn qos(&self) -> QoS {
        self.qos
    }

    /// Return the MQTT v5 response topic, if present.
    pub fn response_topic(&'a self) -> Option<&'a str> {
        self.properties.response_topic()
    }

    /// Return MQTT v5 correlation data, if present.
    pub fn correlation_data(&'a self) -> Option<&'a [u8]> {
        self.properties.correlation_data()
    }

    /// Build a direct reply publication when the inbound message carries a response topic.
    pub fn reply<P>(&'a self, payload: P) -> Option<Publication<'a, P>> {
        Some(ResponseTarget {
            topic: self.response_topic()?,
            correlation_data: self.correlation_data(),
        })
        .map(|target| target.publication(payload))
    }

    /// Copy the response target into fixed-capacity owned storage.
    ///
    /// Use this when the reply has to outlive the borrowed inbound packet.
    pub fn reply_owned<const TOPIC: usize, const CORRELATION: usize>(
        &'a self,
    ) -> Result<Option<OwnedResponseTarget<TOPIC, CORRELATION>>, ProtocolError> {
        match self.response_topic() {
            Some(topic) => ResponseTarget {
                topic,
                correlation_data: self.correlation_data(),
            }
            .to_owned()
            .map(Some),
            None => Ok(None),
        }
    }
}

/// Output of [`Session::poll`](crate::Session::poll).
#[derive(Debug)]
pub enum Event<'a> {
    /// No inbound message was produced.
    Idle,
    /// An inbound publish was received.
    Inbound(InboundPublish<'a>),
}

/// Output of [`Session::connect`](crate::Session::connect).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ConnectEvent {
    /// The broker created a fresh session.
    Connected,
    /// The broker resumed the existing session.
    Reconnected,
}
