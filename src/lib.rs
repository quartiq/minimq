#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

pub mod broker;
pub mod config;
mod de;
mod message_types;
pub mod mqtt_client;
mod packets;
mod properties;
pub mod publication;
mod reason_codes;
mod ser;
pub mod transport;
pub mod types;
mod varint;
mod will;

pub use broker::Broker;
pub use config::{BufferLayout, Buffers, Config, ConfigBuilder, ConfigError};
pub use mqtt_client::{Event, InboundPublish, Session};
pub use properties::Property;
pub use publication::{OwnedResponseTarget, Publication};
pub use reason_codes::ReasonCode;
pub use will::{OwnedWill, Will};

pub use de::Error as DeError;
pub use embedded_io_async;
pub use embedded_io_async::ErrorKind;
pub use embedded_nal_async;
pub use ser::Error as SerError;

use num_enum::TryFromPrimitive;

pub(crate) use log::{debug, error, info, trace};

/// Default port number for unencrypted MQTT traffic.
pub const MQTT_INSECURE_DEFAULT_PORT: u16 = 1883;

/// Default port number for encrypted MQTT traffic.
pub const MQTT_SECURE_DEFAULT_PORT: u16 = 8883;

/// The quality-of-service for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive, PartialOrd, Ord)]
#[repr(u8)]
pub enum QoS {
    AtMostOnce = 0,
    AtLeastOnce = 1,
    ExactlyOnce = 2,
}

/// The retained status for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum Retain {
    NotRetained = 0,
    Retained = 1,
}

/// Errors that are specific to the MQTT protocol implementation.
#[non_exhaustive]
#[derive(Debug, Copy, Clone, PartialEq, thiserror::Error)]
pub enum ProtocolError {
    #[error("provided client ID is too long")]
    ProvidedClientIdTooLong,
    #[error("received an unexpected MQTT packet")]
    UnexpectedPacket,
    #[error("received an invalid MQTT property")]
    InvalidProperty,
    #[error("received a malformed MQTT packet")]
    MalformedPacket,
    #[error("buffer is too small")]
    BufferSize,
    #[error("invalid buffer layout")]
    BufferLayout,
    #[error("unknown packet identifier")]
    BadIdentifier,
    #[error("invalid QoS value")]
    WrongQos,
    #[error("unsupported MQTT packet")]
    UnsupportedPacket,
    #[error("at least one topic is required")]
    NoTopic,
    #[error("authentication was already specified")]
    AuthAlreadySpecified,
    #[error("will message was already specified")]
    WillAlreadySpecified,
    #[error("not connected")]
    NotConnected,
    #[error("in-flight metadata capacity exhausted")]
    InflightMetadataExhausted,
    #[error("broker returned failure reason {0:?}")]
    Failed(ReasonCode),
    #[error(transparent)]
    Serialization(SerError),
    #[error(transparent)]
    Deserialization(DeError),
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum PubError<E> {
    #[error(transparent)]
    Error(#[from] Error),
    #[error("payload serialization failed")]
    Serialization(E),
}

impl<E> From<crate::ser::PubError<E>> for PubError<E> {
    fn from(e: crate::ser::PubError<E>) -> Self {
        match e {
            crate::ser::PubError::Other(e) => Self::Serialization(e),
            crate::ser::PubError::Error(e) => Self::Error(ProtocolError::from(e).into()),
        }
    }
}

impl From<crate::ser::Error> for ProtocolError {
    fn from(err: crate::ser::Error) -> Self {
        Self::Serialization(err)
    }
}

impl From<crate::de::Error> for ProtocolError {
    fn from(err: crate::de::Error) -> Self {
        Self::Deserialization(err)
    }
}

impl From<ReasonCode> for ProtocolError {
    fn from(code: ReasonCode) -> Self {
        Self::Failed(code)
    }
}

/// Possible errors encountered during MQTT operation.
#[derive(Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("session is not ready")]
    NotReady,
    #[error("session is disconnected")]
    Disconnected,
    #[error(transparent)]
    Protocol(ProtocolError),
    #[error("transport error: {0:?}")]
    Transport(ErrorKind),
}

impl From<ProtocolError> for Error {
    fn from(p: ProtocolError) -> Self {
        match p {
            ProtocolError::NotConnected => Self::Disconnected,
            other => Self::Protocol(other),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
