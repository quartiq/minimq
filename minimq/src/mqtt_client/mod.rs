pub(crate) mod outbound;
mod session;

pub use session::{Connection, Session};

use crate::{
    Properties, QoS, ResourceError, Retain,
    publication::{OwnedResponseTarget, Publication, ResponseTarget},
};
use embedded_io_async::{ErrorType, Read, Write};

/// Transport trait required by [`Connection`].
///
/// Outbound packets are written as a burst before flushing. Writes must make progress
/// without requiring an intervening `flush()`, including when the transport's buffer fills.
/// Buffered transports must emit data as needed; adapters that only free space on explicit
/// flush are unsupported. A flush must drain accepted bytes, but need not wait for MQTT ACKs.
///
/// Ordinary lack of inbound data must leave the read future pending. If `read()` returns
/// `TimedOut` or `Interrupted`, [`Connection::poll`] treats that as
/// transport failure and disconnects the connection.
pub trait Io: Read + Write + ErrorType {}

impl<T> Io for T where T: Read + Write + ErrorType {}

/// Handle for one outbound MQTT operation accepted into local session state.
///
/// Query this handle on its originating [`Session`] (or a [`Connection`] borrowing
/// it), and discard it after completion or invalidation. Completion is reported by [`Event::Completed`], including broker rejections.
///
/// Handles use MQTT packet identifiers, which cycle through 1..=65535 and can be
/// reused after an operation ends. Check completion before its identifier is reused:
/// an old handle can then refer to a different operation. Failed requests can also
/// advance identifier allocation.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Op {
    packet_id: u16,
    generation: u32,
}

/// Application-visible progress on an MQTT connection.
#[derive(Debug)]
pub enum Event<'a> {
    /// A publication received from the broker.
    Publish(InboundPublish<'a>),
    /// A broker acknowledgement ended an outbound operation, successfully or otherwise.
    Completed {
        /// The operation accepted by this session.
        op: Op,
        /// The broker's result, including any rejection.
        outcome: Outcome<'a>,
    },
}

/// The broker's result for an outbound operation.
#[derive(Debug)]
pub enum Outcome<'a> {
    /// Completion of a QoS 1 or 2 exchange with the broker.
    Publish(Result<(), crate::ReasonCode>),
    /// Results in the order of the requested topic filters.
    Subscribe(SubscribeResults<'a>),
    /// Acknowledgement of an unsubscribe request.
    Unsubscribe(UnsubscribeResults<'a>),
}

impl Outcome<'_> {
    /// Check that every part of the operation succeeded, returning the first rejection.
    pub fn check(&self) -> Result<(), crate::PeerError> {
        let result = match self {
            Self::Publish(result) => *result,
            Self::Subscribe(results) => results.iter().try_for_each(|result| result.map(|_| ())),
            Self::Unsubscribe(results) => results
                .per_filter()
                .into_iter()
                .flatten()
                .try_for_each(|result| result),
        };
        result.map_err(crate::PeerError::Rejected)
    }
}

/// Validated SUBACK results, borrowed from the receive buffer.
#[derive(Debug)]
pub struct SubscribeResults<'a>(pub(crate) &'a [u8]);

impl SubscribeResults<'_> {
    /// Granted QoS or rejection for each requested topic filter, in request order.
    pub fn iter(&self) -> impl ExactSizeIterator<Item = Result<QoS, crate::ReasonCode>> + '_ {
        self.0.iter().map(|&code| match code {
            0 => Ok(QoS::AtMostOnce),
            1 => Ok(QoS::AtLeastOnce),
            2 => Ok(QoS::ExactlyOnce),
            _ => Err(crate::ReasonCode::from(code)),
        })
    }
}

/// Validated UNSUBACK results, borrowed from the receive buffer.
#[derive(Debug)]
pub struct UnsubscribeResults<'a>(pub(crate) Option<&'a [u8]>);

impl UnsubscribeResults<'_> {
    /// MQTT 5 results in request order. MQTT 3.1.1 acknowledges the whole request
    /// successfully and supplies no per-filter results, returning `None` here.
    pub fn per_filter(
        &self,
    ) -> Option<impl ExactSizeIterator<Item = Result<(), crate::ReasonCode>> + '_> {
        self.0.map(|codes| {
            codes
                .iter()
                .map(|&code| broker_result(crate::ReasonCode::from(code)))
        })
    }
}

pub(crate) fn broker_result(code: crate::ReasonCode) -> Result<(), crate::ReasonCode> {
    if code.success() { Ok(()) } else { Err(code) }
}

impl Op {
    pub(crate) fn new(packet_id: u16, generation: u32) -> Self {
        Self {
            packet_id,
            generation,
        }
    }
}

/// Inbound MQTT `PUBLISH` surfaced by [`Connection::poll`] and by
/// [`Connection::drive`] / [`Connection::poll`] when they return `Some(...)`.
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
    SessionCreated,
    /// The broker resumed the existing session.
    SessionResumed,
}
