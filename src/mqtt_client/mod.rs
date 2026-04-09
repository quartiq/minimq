mod core;
mod session;

pub use session::Session;

use crate::{
    ProtocolError, QoS, Retain,
    publication::{OwnedResponseTarget, Publication, ResponseTarget},
    types::Properties,
};

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

    pub fn response_target(&'a self) -> Option<ResponseTarget<'a>> {
        Some(ResponseTarget {
            topic: self.response_topic()?,
            correlation_data: self.correlation_data(),
        })
    }

    pub fn reply<P>(&'a self, payload: P) -> Option<Publication<'a, P>> {
        self.response_target()
            .map(|target| target.publication(payload))
    }

    pub fn reply_owned<const TOPIC: usize, const CORRELATION: usize>(
        &'a self,
    ) -> Result<Option<OwnedResponseTarget<TOPIC, CORRELATION>>, ProtocolError> {
        self.response_target()
            .map(|target| target.to_owned())
            .transpose()
    }
}

#[derive(Debug)]
pub enum Event<'a> {
    Idle,
    Connected,
    Reconnected,
    Inbound(InboundPublish<'a>),
}
