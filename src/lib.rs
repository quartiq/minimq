#![no_std]
//! # MiniMQ
//! Provides a minimal MQTTv5 client and message parsing for the MQTT version 5 protocol.
//!
//! This crate provides a minimalistic MQTT 5 client that can be used to publish topics to an MQTT
//! broker and subscribe to receive messages on specific topics.
//!
//! # Limitations
//! This library does not currently support the following elements:
//! * Quality-of-service `ExactlyOnce`
//! * Session timeouts
//! * Server keep alive timeouts (ping)
//! * Bulk subscriptions
//! * Server Authentication
//! * Encryption
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
//! Below is a sample snippet showing how this library is used. An example application is provided
//! in `examples/minimq-stm32h7`, which targets the Nucleo-H743 development board with an external
//! temperature sensor installed.
//!
//! ```no_run
//! use minimq::{Minimq, QoS};
//!
//! // Construct an MQTT client with a maximum packet size of 256 bytes.
//! // Connect to a broker at localhost - Use a client ID of "test".
//! let mut mqtt: Minimq<_, _, 256, 16> = Minimq::new(
//!         "127.0.0.1".parse().unwrap(),
//!         "test",
//!         std_embedded_nal::Stack::default(),
//!         std_embedded_time::StandardClock::default()).unwrap();
//!
//! let mut subscribed = false;
//!
//! loop {
//!     if mqtt.client.is_connected() && !subscribed {
//!         mqtt.client.subscribe("topic", &[]).unwrap();
//!         subscribed = true;
//!     }
//!
//!     mqtt.poll(|client, topic, message, properties| {
//!         match topic {
//!             "topic" => {
//!                println!("{:?}", message);
//!                client.publish("echo", message, QoS::AtMostOnce, &[]).unwrap();
//!             },
//!             topic => println!("Unknown topic: {}", topic),
//!         };
//!     }).unwrap();
//! }
//! ```

pub(crate) mod de;
pub(crate) mod ser;

mod message_types;
mod mqtt_client;
mod network_manager;
mod properties;
mod session_state;
mod will;

use message_types::MessageType;
pub use properties::Property;

pub use embedded_nal;
pub use embedded_time;
pub use mqtt_client::Minimq;

#[cfg(feature = "logging")]
pub(crate) use log::{debug, error, info, warn};

/// The quality-of-service for an MQTT message.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum QoS {
    /// A packet will be delivered at most once, but may not be delivered at all.
    AtMostOnce = 0,

    /// A packet will be delivered at least one time, but possibly more than once.
    AtLeastOnce = 1,

    /// A packet will be delivered exactly one time.
    ExactlyOnce = 2,
}

/// Errors that are specific to the MQTT protocol implementation.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum ProtocolError {
    Bounds,
    DataSize,
    Invalid,
    Failed,
    PacketSize,
    MalformedPacket,
    MalformedInteger,
    UnknownProperty,
    UnsupportedPacket,
    BufferSize,
    InvalidProperty,
}

/// Possible errors encountered during an MQTT connection.
#[derive(Debug, PartialEq)]
pub enum Error<E> {
    Network(E),
    WriteFail,
    NotReady,
    Unsupported,
    ProvidedClientIdTooLong,
    Failed(u8),
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
