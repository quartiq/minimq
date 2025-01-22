#![cfg_attr(not(test), no_std)]
//! # MiniMQ
//! Provides a minimal MQTTv5 client and message parsing for the MQTT version 5 protocol.
//!
//! This crate provides a minimalistic MQTT 5 client that can be used to publish topics to an MQTT
//! broker and subscribe to receive messages on specific topics.
//!
//! # Limitations
//! This library does not currently support the following elements:
//! * Subscribing above Quality-of-service `AtMostOnce`
//! * Server Authentication
//! * Topic aliases
//!
//! # Requirements
//! This library requires that the user provide it an object that implements a basic TcpStack that
//! can be used as the transport layer for MQTT communications.
//!
//! The maximum message size is configured through generic parameters. This allows the maximum
//! message size to be configured by the user. Note that buffers will be allocated on the stack, so it
//! is important to select a size such that the stack does not overflow.
//!
//! # Example
//! Below is a sample snippet showing how this library is used.
//!
//! ```no_run
//! use minimq::{ConfigBuilder, Minimq, Publication};
//!
//! // Construct an MQTT client with a maximum packet size of 256 bytes
//! // and a maximum of 16 messages that are allowed to be "in flight".
//! // Messages are "in flight" if QoS::AtLeastOnce has not yet been acknowledged (PUBACK)
//! // or QoS::ExactlyOnce has not been completed (PUBCOMP).
//! // Connect to a broker at localhost - Use a client ID of "test".
//! let mut buffer = [0; 256];
//! let localhost: std::net::IpAddr = "127.0.0.1".parse().unwrap();
//! let mut mqtt: Minimq<'_, _, _, minimq::broker::IpBroker> = Minimq::new(
//!         std_embedded_nal::Stack::default(),
//!         std_embedded_time::StandardClock::default(),
//!         ConfigBuilder::new(localhost.into(), &mut buffer)
//!             .client_id("test").unwrap(),
//!         );
//!
//! let mut subscribed = false;
//!
//! loop {
//!     if mqtt.client().is_connected() && !subscribed {
//!         mqtt.client().subscribe(&["topic".into()], &[]).unwrap();
//!         subscribed = true;
//!     }
//!
//!     // The client must be continually polled to update the MQTT state machine.
//!     mqtt.poll(|client, topic, message, properties| {
//!         match topic {
//!             "topic" => {
//!                println!("{:?}", message);
//!                client.publish(Publication::new("echo", message)).unwrap();
//!             },
//!             topic => println!("Unknown topic: {}", topic),
//!         };
//!     }).unwrap();
//! }
//! ```

pub mod broker;
pub mod config;
mod de;
mod message_types;
pub mod mqtt_client;
mod network_manager;
mod packets;
mod properties;
pub mod publication;
mod reason_codes;
mod republication;
mod ring_buffer;
mod ser;
mod session_state;
pub mod types;
mod varint;
mod will;

pub use broker::Broker;
pub use config::ConfigBuilder;
pub use properties::Property;
pub use publication::{Deferred, Publication};
pub use reason_codes::ReasonCode;
pub use will::Will;

pub use embedded_nal;
pub use embedded_time;
pub use mqtt_client::Minimq;
use num_enum::TryFromPrimitive;

pub use de::Error as DeError;
pub use ser::Error as SerError;

#[cfg(feature = "logging")]
pub(crate) use log::{debug, error, info, trace, warn};

/// Default port number for unencrypted MQTT traffic
///
/// # Note:
/// See [IANA Port Numbers](https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.txt)
pub const MQTT_INSECURE_DEFAULT_PORT: u16 = 1883;

/// Default port number for encrypted MQTT traffic
///
/// # Note:
/// See [IANA Port Numbers](https://www.iana.org/assignments/service-names-port-numbers/service-names-port-numbers.txt)
pub const MQTT_SECURE_DEFAULT_PORT: u16 = 8883;

/// The quality-of-service for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq, TryFromPrimitive, PartialOrd)]
#[repr(u8)]
pub enum QoS {
    /// A packet will be delivered at most once, but may not be delivered at all.
    AtMostOnce = 0,

    /// A packet will be delivered at least one time, but possibly more than once.
    AtLeastOnce = 1,

    /// A packet will be delivered exactly one time.
    ExactlyOnce = 2,
}

/// The retained status for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq, TryFromPrimitive)]
#[repr(u8)]
pub enum Retain {
    /// The message shall not be retained by the broker.
    NotRetained = 0,

    /// The message shall be marked for retention by the broker.
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
    BadIdentifier,
    Unacknowledged,
    WrongQos,
    UnsupportedPacket,
    NoTopic,
    AuthAlreadySpecified,
    WillAlreadySpecified,
    Failed(ReasonCode),
    Serialization(SerError),
    Deserialization(DeError),
}

#[derive(Debug, PartialEq)]
pub enum PubError<T, E> {
    Error(Error<T>),
    Serialization(E),
}

impl<T, E> From<crate::ser::PubError<E>> for PubError<T, E> {
    fn from(e: crate::ser::PubError<E>) -> Self {
        match e {
            crate::ser::PubError::Other(e) => crate::PubError::Serialization(e),
            crate::ser::PubError::Error(e) => crate::PubError::Error(crate::Error::Minimq(
                crate::MinimqError::Protocol(ProtocolError::from(e)),
            )),
        }
    }
}

impl<T, E> From<Error<T>> for PubError<T, E> {
    fn from(e: Error<T>) -> Self {
        Self::Error(e)
    }
}

impl From<crate::ser::Error> for ProtocolError {
    fn from(err: crate::ser::Error) -> Self {
        ProtocolError::Serialization(err)
    }
}

impl From<crate::de::Error> for ProtocolError {
    fn from(err: crate::de::Error) -> Self {
        ProtocolError::Deserialization(err)
    }
}

impl From<ReasonCode> for ProtocolError {
    fn from(code: ReasonCode) -> Self {
        ProtocolError::Failed(code)
    }
}

#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum MinimqError {
    Protocol(ProtocolError),
    Clock(embedded_time::clock::Error),
}

/// Possible errors encountered during an MQTT connection.
#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum Error<E> {
    WriteFail,
    NotReady,
    Unsupported,
    NoResponseTopic,
    SessionReset,
    Network(E),
    Minimq(MinimqError),
}

impl<E> From<MinimqError> for Error<E> {
    fn from(minimq: MinimqError) -> Self {
        Error::Minimq(minimq)
    }
}

impl From<embedded_time::clock::Error> for MinimqError {
    fn from(clock: embedded_time::clock::Error) -> Self {
        MinimqError::Clock(clock)
    }
}

impl<E> From<ProtocolError> for Error<E> {
    fn from(p: ProtocolError) -> Self {
        Error::Minimq(p.into())
    }
}

impl<E> From<embedded_time::clock::Error> for Error<E> {
    fn from(clock: embedded_time::clock::Error) -> Self {
        Error::Minimq(clock.into())
    }
}

impl From<ProtocolError> for MinimqError {
    fn from(error: ProtocolError) -> Self {
        MinimqError::Protocol(error)
    }
}

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
