#![no_std]
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
//! use minimq::{Minimq, Publication};
//!
//! // Construct an MQTT client with a maximum packet size of 256 bytes
//! // and a maximum of 16 messages that are allowed to be "in flight".
//! // Messages are "in flight" if QoS::AtLeastOnce has not yet been acknowledged (PUBACK)
//! // or QoS::ExactlyOnce has not been completed (PUBCOMP).
//! // Connect to a broker at localhost - Use a client ID of "test".
//! let mut rx_buffer = [0; 256];
//! let mut tx_buffer = [0; 256];
//! let mut mqtt: Minimq<_, _, 256, 16> = Minimq::new(
//!         "127.0.0.1".parse().unwrap(),
//!         "test",
//!         std_embedded_nal::Stack::default(),
//!         std_embedded_time::StandardClock::default(),
//!         &mut rx_buffer,
//!         &mut tx_buffer).unwrap();
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
//!                let response = Publication::new(message).topic("echo").finish().unwrap();
//!                client.publish(response).unwrap();
//!             },
//!             topic => println!("Unknown topic: {}", topic),
//!         };
//!     }).unwrap();
//! }
//! ```

mod de;
mod ser;

mod message_types;
pub mod mqtt_client;
mod network_manager;
mod packets;
mod properties;
pub mod publication;
mod reason_codes;
mod session_state;
pub mod types;
mod varint;
mod will;

pub use properties::Property;
pub use publication::Publication;
pub use reason_codes::ReasonCode;
pub use will::Will;

pub use embedded_nal;
pub use embedded_time;
pub use mqtt_client::Minimq;
use num_enum::TryFromPrimitive;

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
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ProtocolError {
    PacketSize,
    MalformedPacket,
    BufferSize,
    InvalidProperty,
    BadIdentifier,
    Unacknowledged,
    WrongQos,
    UnsupportedPacket,
    NoTopic,
    Serialization(crate::ser::Error),
    Deserialization(crate::de::Error),
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

impl<E> From<crate::ser::Error> for Error<E> {
    fn from(err: crate::ser::Error) -> Self {
        Error::Protocol(ProtocolError::Serialization(err))
    }
}

impl<E> From<crate::de::Error> for Error<E> {
    fn from(err: crate::de::Error) -> Self {
        Error::Protocol(ProtocolError::Deserialization(err))
    }
}

impl<E> From<ReasonCode> for Error<E> {
    fn from(code: ReasonCode) -> Self {
        Error::Failed(code)
    }
}

/// Possible errors encountered during an MQTT connection.
#[derive(Debug, PartialEq)]
pub enum Error<E> {
    Network(E),
    WriteFail,
    NotReady,
    Unsupported,
    ProvidedClientIdTooLong,
    NoResponseTopic,
    Failed(ReasonCode),
    Protocol(ProtocolError),
    SessionReset,
    Clock(embedded_time::clock::Error),
}

impl<E> From<embedded_time::clock::Error> for Error<E> {
    fn from(clock: embedded_time::clock::Error) -> Self {
        Error::Clock(clock)
    }
}

impl<E> From<ProtocolError> for Error<E> {
    fn from(error: ProtocolError) -> Self {
        Error::Protocol(error)
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
