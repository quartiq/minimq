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

#[derive(Debug)]
pub struct InboundPublish<'a> {
    pub topic: &'a str,
    pub payload: &'a [u8],
    pub properties: Properties<'a>,
    pub retain: Retain,
    pub qos: QoS,
}

impl<'a> InboundPublish<'a> {
    pub fn response_topic(&'a self) -> Option<&'a str> {
        self.properties.response_topic()
    }

    pub fn correlation_data(&'a self) -> Option<&'a [u8]> {
        self.properties.correlation_data()
    }

    pub fn reply<P>(&'a self, payload: P) -> Option<Publication<'a, P>> {
        Some(ResponseTarget {
            topic: self.response_topic()?,
            correlation_data: self.correlation_data(),
        })
        .map(|target| target.publication(payload))
    }

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

#[derive(Debug)]
pub enum Event<'a> {
    Idle,
    Connected,
    Reconnected,
    Inbound(InboundPublish<'a>),
}
