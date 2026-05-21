mod outbound;
mod session;

pub use session::Session;

use crate::{
    Properties, QoS, ResourceError, Retain,
    publication::{OwnedResponseTarget, Publication, ResponseTarget},
};
use embedded_io_async::{ErrorType, Read, Write};

/// Transport trait required by [`Session`](crate::Session).
///
/// Ordinary lack of inbound data must leave the read future pending. If `read()` returns
/// `TimedOut` or `Interrupted`, [`Session::poll`](crate::Session::poll) treats that as transport
/// failure and disconnects the session.
pub trait Io: Read + Write + ErrorType {}

impl<T> Io for T where T: Read + Write + ErrorType {}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum OpKind {
    PublishAtLeastOnce,
    PublishExactlyOnce,
    Subscribe,
    Unsubscribe,
}

/// Handle for one outbound MQTT operation accepted into local session state.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Op {
    kind: OpKind,
    packet_id: u16,
    generation: u32,
}

impl Op {
    pub(crate) fn new(kind: OpKind, packet_id: u16, generation: u32) -> Self {
        Self {
            kind,
            packet_id,
            generation,
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum OpStatus {
    /// The operation is still present in local in-flight state.
    Pending,
    /// The operation is no longer pending in this session generation.
    Complete,
    /// The local session state that could complete this operation was discarded.
    Invalidated,
}

/// Inbound MQTT `PUBLISH` surfaced by [`Session::recv`](crate::Session::recv) and by
/// [`Session::drive`](crate::Session::drive) / [`Session::poll`](crate::Session::poll) when they
/// return `Some(...)`.
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

    /// Return whether the broker marked this message as retained.
    pub const fn retained(&self) -> bool {
        matches!(self.retain, Retain::Retained)
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

    fn response_target(&'a self) -> Option<ResponseTarget<'a>> {
        Some(ResponseTarget {
            topic: self.response_topic()?,
            correlation_data: self.correlation_data(),
        })
    }

    /// Build a direct reply publication when the inbound message carries a response topic.
    pub fn reply<P>(&'a self, payload: P) -> Option<Publication<'a, P>> {
        self.response_target()
            .map(|target| target.publication(payload))
    }

    /// Copy the response target into fixed-capacity owned storage.
    ///
    /// Use this when the reply has to outlive the borrowed inbound packet.
    pub fn reply_owned<const TOPIC: usize, const CORRELATION: usize>(
        &'a self,
    ) -> Result<Option<OwnedResponseTarget<TOPIC, CORRELATION>>, ResourceError> {
        self.response_target()
            .map(ResponseTarget::to_owned)
            .transpose()
    }
}

/// Output of [`Session::connect`](crate::Session::connect).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ConnectEvent {
    /// The broker created a fresh session.
    Connected,
    /// The broker resumed the existing session.
    Reconnected,
}
