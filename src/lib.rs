#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

/// Session configuration and caller-owned buffers.
pub mod config;
mod de;
mod message_types;
/// Long-lived MQTT client session types.
pub mod mqtt_client;
mod packets;
mod properties;
/// Outbound publish builders and payload adapters.
pub mod publication;
mod reason_codes;
mod ser;
/// MQTT-specific value types used by the public API.
pub mod types;
mod varint;
mod will;

pub use config::{Buffers, ConfigBuilder, SetupError};
pub use mqtt_client::{ConnectEvent, InboundPublish, Io, Session};
pub use properties::Property;
pub use publication::{OwnedResponseTarget, Publication};
pub use reason_codes::ReasonCode;
pub use will::Will;

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;

pub use de::Error as DeError;
pub use ser::Error as SerError;

use num_enum::TryFromPrimitive;

pub(crate) use log::{debug, error, info, trace, warn};

/// Session error type for a specific transport.
pub type SessionError<IO> = Error<<IO as embedded_io_async::ErrorType>::Error>;

/// Publish error type for a specific transport and payload serializer.
pub type PublishError<IO, P> = PubError<P, <IO as embedded_io_async::ErrorType>::Error>;

/// Default port number for unencrypted MQTT traffic.
pub const MQTT_INSECURE_DEFAULT_PORT: u16 = 1883;

/// Default port number for encrypted MQTT traffic.
pub const MQTT_SECURE_DEFAULT_PORT: u16 = 8883;

/// The quality-of-service for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive, PartialOrd, Ord)]
#[repr(u8)]
pub enum QoS {
    /// Deliver at most once. No acknowledgment or retry.
    AtMostOnce = 0,
    /// Deliver at least once. Retries are possible.
    AtLeastOnce = 1,
    /// Deliver exactly once through the MQTT QoS 2 handshake.
    ExactlyOnce = 2,
}

/// The retained status for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum Retain {
    /// Do not retain the message on the broker.
    NotRetained = 0,
    /// Ask the broker to retain the message.
    Retained = 1,
}

/// Errors that are specific to the MQTT protocol implementation.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ProtocolError {
    /// The configured client identifier exceeds the internal fixed-capacity storage.
    #[error("provided client ID is too long")]
    ProvidedClientIdTooLong,
    /// The broker sent a packet that is invalid in the current protocol state.
    #[error("received an unexpected MQTT packet")]
    UnexpectedPacket,
    /// A property was not valid for that packet type or operation.
    #[error("received an invalid MQTT property")]
    InvalidProperty,
    /// The broker sent malformed bytes.
    #[error("received a malformed MQTT packet")]
    MalformedPacket,
    /// Fixed-capacity storage was too small for the requested value.
    #[error("buffer is too small")]
    BufferSize,
    /// The requested RX/TX split exceeds the provided backing buffer.
    #[error("invalid buffer split")]
    BufferSplit,
    /// The broker referred to an unknown packet identifier.
    #[error("unknown packet identifier")]
    BadIdentifier,
    /// A QoS value was outside the MQTT-defined range.
    #[error("invalid QoS value")]
    WrongQos,
    /// `minimq` does not implement that MQTT packet or feature.
    #[error("unsupported MQTT packet")]
    UnsupportedPacket,
    /// An operation that requires at least one topic was called with none.
    #[error("at least one topic is required")]
    NoTopic,
    /// Authentication was configured more than once.
    #[error("authentication was already specified")]
    AuthAlreadySpecified,
    /// A will was configured more than once.
    #[error("will message was already specified")]
    WillAlreadySpecified,
    /// The operation requires an active MQTT connection.
    #[error("not connected")]
    NotConnected,
    /// Internal tracking space for in-flight packet metadata was exhausted.
    #[error("in-flight metadata capacity exhausted")]
    InflightMetadataExhausted,
    /// The broker rejected the operation with an MQTT reason code.
    #[error("broker returned failure reason {0:?}")]
    Failed(ReasonCode),
    /// Packet encoding failed.
    #[error(transparent)]
    Encode(SerError),
    /// Packet decoding failed.
    #[error(transparent)]
    Deserialization(DeError),
}

/// Error returned from [`Session::publish`](crate::Session::publish).
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum PubError<P, E> {
    /// Session setup, transport, or protocol failure.
    #[error(transparent)]
    Session(#[from] Error<E>),
    /// Payload serialization failed before the packet was sent.
    #[error("payload serialization failed")]
    Payload(P),
}

impl<P, E> From<crate::ser::PubError<P>> for PubError<P, E> {
    fn from(e: crate::ser::PubError<P>) -> Self {
        match e {
            crate::ser::PubError::Payload(e) => Self::Payload(e),
            crate::ser::PubError::Encode(e) => Self::Session(ProtocolError::from(e).into()),
        }
    }
}

impl From<crate::ser::Error> for ProtocolError {
    fn from(err: crate::ser::Error) -> Self {
        Self::Encode(err)
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
pub enum Error<E> {
    /// Local buffers or in-flight state are not currently ready for the requested operation.
    #[error("session is not ready")]
    NotReady,
    /// The session is currently disconnected.
    #[error("session is disconnected")]
    Disconnected,
    /// MQTT protocol-level failure.
    #[error(transparent)]
    Protocol(ProtocolError),
    /// Transport-layer failure during connect or I/O.
    #[error("transport error: {0:?}")]
    Transport(E),
    /// A write operation returned `Ok(0)` for a non-empty buffer.
    #[error("transport write returned zero bytes")]
    WriteZero,
}

impl<E> From<ProtocolError> for Error<E> {
    fn from(p: ProtocolError) -> Self {
        match p {
            ProtocolError::NotConnected => Self::Disconnected,
            other => Self::Protocol(other),
        }
    }
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
pub(crate) mod tests;
