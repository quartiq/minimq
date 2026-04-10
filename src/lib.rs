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

#[cfg(feature = "logging")]
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
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ProtocolError {
    ProvidedClientIdTooLong,
    UnexpectedPacket,
    InvalidProperty,
    MalformedPacket,
    BufferSize,
    BufferLayout,
    BadIdentifier,
    WrongQos,
    UnsupportedPacket,
    NoTopic,
    AuthAlreadySpecified,
    WillAlreadySpecified,
    NotConnected,
    InflightMetadataExhausted,
    Failed(ReasonCode),
    Serialization(SerError),
    Deserialization(DeError),
}

#[derive(Debug, PartialEq)]
pub enum PubError<E> {
    Error(Error),
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

impl<E> From<Error> for PubError<E> {
    fn from(e: Error) -> Self {
        Self::Error(e)
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
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum Error {
    NotReady,
    Disconnected,
    Protocol(ProtocolError),
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

#[doc(hidden)]
#[cfg(not(feature = "logging"))]
mod mqtt_log {
    #[doc(hidden)]
    #[macro_export]
    macro_rules! trace {
        ($($arg:tt)+) => {
            ()
        };
    }

    #[doc(hidden)]
    #[macro_export]
    macro_rules! debug {
        ($($arg:tt)+) => {
            ()
        };
    }

    #[doc(hidden)]
    #[macro_export]
    macro_rules! info {
        ($($arg:tt)+) => {
            ()
        };
    }

    #[doc(hidden)]
    #[macro_export]
    macro_rules! warn {
        ($($arg:tt)+) => {
            ()
        };
    }

    #[doc(hidden)]
    #[macro_export]
    macro_rules! error {
        ($($arg:tt)+) => {
            ()
        };
    }
}
