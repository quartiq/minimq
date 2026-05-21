#![cfg_attr(not(test), no_std)]
#![doc = include_str!("../README.md")]

mod config;
mod de;
mod message_types;
mod mqtt_client;
mod packets;
mod properties;
mod publication;
mod reason_codes;
mod ser;
mod types;
mod varint;
mod will;

pub use config::{Buffers, ConfigBuilder};
pub use mqtt_client::{ConnectEvent, InboundPublish, Io, Op, OpStatus, Session};
pub use packets::Disconnect;
pub use properties::Property;
pub use publication::{OwnedResponseTarget, Publication, ToPayload};
pub use reason_codes::ReasonCode;
pub use types::{Properties, RetainHandling, SubscriptionOptions, TopicFilter};
pub use will::Will;

#[cfg(feature = "fuzzing")]
#[doc(hidden)]
pub mod fuzzing;

use de::Error as DeError;
use ser::Error as SerError;

use num_enum::TryFromPrimitive;

pub(crate) use defmt::{debug, error, info, trace, warn};

/// Session error type for a specific transport.
pub type SessionError<IO> = Error<<IO as embedded_io_async::ErrorType>::Error>;

/// Publish error type for a specific transport and payload serializer.
pub type PublishError<IO, P> = PubError<P, <IO as embedded_io_async::ErrorType>::Error>;

/// Default port number for unencrypted MQTT traffic.
pub const MQTT_INSECURE_DEFAULT_PORT: u16 = 1883;

/// Default port number for encrypted MQTT traffic.
pub const MQTT_SECURE_DEFAULT_PORT: u16 = 8883;

/// The quality-of-service for an MQTT message.
#[derive(defmt::Format, Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive, PartialOrd, Ord)]
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
#[derive(defmt::Format, Debug, Copy, Clone, PartialEq, Eq, TryFromPrimitive)]
#[repr(u8)]
pub enum Retain {
    /// Do not retain the message on the broker.
    NotRetained = 0,
    /// Ask the broker to retain the message.
    Retained = 1,
}

/// Configuration errors detected before a session is created.
#[derive(defmt::Format, Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ConfigError {
    /// The requested RX split does not fit in the provided backing buffer.
    #[error("buffer split exceeds backing storage")]
    BufferSplit,
    /// The configured client identifier exceeds the internal fixed-capacity storage.
    #[error("provided client ID is too long")]
    ClientIdTooLong,
    /// The configured topic exceeds the internal fixed-capacity storage.
    #[error("provided topic is too long")]
    TopicTooLong,
    /// One configuration setting was specified more than once.
    #[error("configuration was specified more than once")]
    DuplicateConfig,
    /// The provided configuration is not valid for MQTT.
    #[error("invalid MQTT configuration")]
    InvalidConfig,
}

/// Failures caused by broker behavior or invalid inbound MQTT data.
#[derive(defmt::Format, Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PeerError {
    /// The broker explicitly rejected the operation with an MQTT reason code.
    #[error("broker returned failure reason {0:?}")]
    Rejected(ReasonCode),
    /// The broker sent an invalid MQTT packet or protocol state transition.
    #[error("received an invalid MQTT packet")]
    InvalidPacket,
}

/// Local capacity and sizing failures.
#[derive(defmt::Format, Debug, Copy, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ResourceError {
    /// Local fixed-capacity storage or packet scratch space was too small.
    #[error("buffer is too small")]
    BufferTooSmall,
    /// The requested or required packet exceeds the negotiated packet size limit.
    #[error("packet is too large")]
    PacketTooLarge,
    /// Internal tracking space for in-flight packet metadata was exhausted.
    #[error("in-flight metadata capacity exhausted")]
    InflightExhausted,
}

/// Error returned from [`Session::publish`](crate::Session::publish).
#[derive(Debug, PartialEq, thiserror::Error)]
pub enum PubError<P, E> {
    /// Session, transport, peer, or local resource failure.
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
            crate::ser::PubError::Encode(e) => Self::Session(Error::from(e)),
        }
    }
}

impl<P, E> From<ProtocolError> for PubError<P, E> {
    fn from(err: ProtocolError) -> Self {
        Self::Session(err.into())
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
    /// The requested operation arguments are not valid.
    #[error("invalid request")]
    InvalidRequest,
    /// The broker rejected the operation or sent invalid MQTT data.
    #[error(transparent)]
    Peer(PeerError),
    /// Local buffers or in-flight state were insufficient for the requested operation.
    #[error(transparent)]
    Resource(ResourceError),
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
            ProtocolError::UnexpectedPacket
            | ProtocolError::MalformedPacket
            | ProtocolError::Deserialization(_) => Self::Peer(PeerError::InvalidPacket),
            ProtocolError::InflightMetadataExhausted => {
                Self::Resource(ResourceError::InflightExhausted)
            }
            ProtocolError::Failed(code) => match code {
                ReasonCode::PacketTooLarge => Self::Resource(ResourceError::PacketTooLarge),
                code => Self::Peer(PeerError::Rejected(code)),
            },
            ProtocolError::Encode(err) => Self::from(err),
        }
    }
}

impl<E> From<crate::ser::Error> for Error<E> {
    fn from(err: crate::ser::Error) -> Self {
        match err {
            crate::ser::Error::InsufficientMemory => Self::Resource(ResourceError::BufferTooSmall),
            crate::ser::Error::Custom => Self::InvalidRequest,
        }
    }
}

impl<E> From<crate::de::Error> for Error<E> {
    fn from(err: crate::de::Error) -> Self {
        let _ = err;
        Self::Peer(PeerError::InvalidPacket)
    }
}

impl<E> From<PeerError> for Error<E> {
    fn from(err: PeerError) -> Self {
        Self::Peer(err)
    }
}

impl<E> From<ResourceError> for Error<E> {
    fn from(err: ResourceError) -> Self {
        Self::Resource(err)
    }
}

#[derive(defmt::Format, Debug, Clone, PartialEq, thiserror::Error)]
pub(crate) enum ProtocolError {
    /// The broker sent a packet that is invalid in the current protocol state.
    #[error("received an unexpected MQTT packet")]
    UnexpectedPacket,
    /// The broker sent malformed bytes.
    #[error("received a malformed MQTT packet")]
    MalformedPacket,
    /// Internal tracking space for in-flight packet metadata was exhausted.
    #[error("in-flight metadata capacity exhausted")]
    InflightMetadataExhausted,
    /// The broker rejected the operation with an MQTT reason code.
    #[error("broker returned failure reason {0:?}")]
    Failed(ReasonCode),
    /// Packet encoding failed.
    #[error(transparent)]
    Encode(#[from] SerError),
    /// Packet decoding failed.
    #[error(transparent)]
    Deserialization(#[from] DeError),
}

impl From<ReasonCode> for ProtocolError {
    fn from(code: ReasonCode) -> Self {
        Self::Failed(code)
    }
}

#[cfg(test)]
#[path = "../tests/support/mod.rs"]
pub(crate) mod tests;
