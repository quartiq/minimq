mod core;
mod outbound;
mod protocol;
mod session;

pub use session::Session;

use embedded_io_async::{ErrorType, Read, Write};

use crate::{
    ProtocolError, QoS, Retain,
    publication::{OwnedResponseTarget, Publication, ResponseTarget},
    types::Properties,
};

pub(super) trait Io: Read + Write + ErrorType
where
    Self::Error: embedded_io_async::Error,
{
}

impl<T> Io for T
where
    T: Read + Write + ErrorType,
    T::Error: embedded_io_async::Error,
{
}

/// Inbound MQTT `PUBLISH` delivered by [`Event::Inbound`].
#[derive(Debug)]
pub struct InboundPublish<'a> {
    /// Topic name from the broker.
    pub topic: &'a str,
    /// Borrowed payload bytes.
    pub payload: &'a [u8],
    /// MQTT v5 publish properties.
    pub properties: Properties<'a>,
    /// Retain flag from the inbound publish.
    pub retain: Retain,
    /// QoS level from the inbound publish.
    pub qos: QoS,
}

impl<'a> InboundPublish<'a> {
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
    /// The transport connected and the broker created a fresh session.
    Connected,
    /// The transport reconnected and the broker resumed the existing session.
    Reconnected,
    /// An inbound publish was received.
    Inbound(InboundPublish<'a>),
}
