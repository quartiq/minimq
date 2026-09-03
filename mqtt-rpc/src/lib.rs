#![no_std]
#![warn(missing_docs)]

//! MQTT 5 request/response transport for embedded services.
//!
//! `mqtt-rpc` owns neither the MQTT session nor application dispatch. It subscribes one
//! `<device-prefix>/rpc/#` topic filter, extracts MQTT response topics and correlation data from
//! matching requests, and sends transient correlated responses. Applications remain responsible
//! for request payloads, method dispatch, execution, and durable state publication.

use heapless::String;
use minimq::{
    ConnectEvent, Connection, Error as MqttError, InboundPublish, Io, Op, OwnedResponseTarget,
    Property, PubError, QoS, ResourceError, RetainHandling, SubscriptionOptions, ToPayload,
    TopicFilter,
};

/// Maximum request and response topic length retained by the service.
pub const MAX_TOPIC_LENGTH: usize = 128;

/// Maximum MQTT correlation-data length retained for a deferred response.
pub const MAX_CORRELATION_LENGTH: usize = 32;

/// Expiry applied to transient RPC responses.
pub const RESPONSE_EXPIRY_SECS: u32 = 30;

/// MQTT user-property name carrying an RPC response code.
pub const RESPONSE_CODE_PROPERTY: &str = "code";

/// Response code indicating successful request execution.
pub const SUCCESS_CODE: &str = "Ok";

const RPC_SUFFIX: &str = "/rpc";
const RPC_FILTER_SUFFIX: &str = "/#";

/// An owned MQTT response destination and optional correlation data.
pub type ResponseTarget = OwnedResponseTarget<MAX_TOPIC_LENGTH, MAX_CORRELATION_LENGTH>;

/// Invalid service configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigError {
    /// The resulting RPC subscription topic exceeds [`MAX_TOPIC_LENGTH`].
    TopicTooLong,
}

/// Why an inbound RPC publication was rejected before application dispatch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectReason {
    /// RPC requests must not be retained broker state.
    Retained,
    /// The request did not provide an MQTT response topic.
    MissingResponseTopic,
    /// The response topic or correlation data exceeds fixed local storage.
    ResponseTargetTooLong,
    /// The request addressed `/rpc` without a method path.
    EmptyMethod,
}

impl RejectReason {
    /// Return the stable response code for this rejection.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Retained => "Retained",
            Self::MissingResponseTopic => "MissingResponseTopic",
            Self::ResponseTargetTooLong => "ResponseTargetTooLong",
            Self::EmptyMethod => "EmptyMethod",
        }
    }
}

/// A valid request borrowing its method and payload from the inbound MQTT packet.
#[derive(Debug)]
pub struct Request<'a> {
    method: &'a str,
    payload: &'a [u8],
    response: ResponseTarget,
}

impl<'a> Request<'a> {
    /// Return the method topic suffix following `<device-prefix>/rpc/`.
    pub const fn method(&self) -> &'a str {
        self.method
    }

    /// Return the request payload.
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }

    /// Consume the request and retain its response destination for deferred completion.
    pub fn into_response_target(self) -> ResponseTarget {
        self.response
    }
}

/// A rejected request and any response destination that could safely be retained.
#[derive(Debug)]
pub struct Rejected {
    reason: RejectReason,
    response: Option<ResponseTarget>,
}

impl Rejected {
    /// Return the rejection reason.
    pub const fn reason(&self) -> RejectReason {
        self.reason
    }

    /// Consume the rejection and return its response destination, when available.
    pub fn into_response_target(self) -> Option<ResponseTarget> {
        self.response
    }
}

/// Classification of one inbound MQTT publication.
#[derive(Debug)]
pub enum Handle<'a> {
    /// The publication is outside this service's RPC topic tree.
    Unhandled,
    /// The publication belongs to the service but is not a valid request.
    Rejected(Rejected),
    /// A valid request ready for application dispatch.
    Request(Request<'a>),
}

/// MQTT RPC topic routing and subscription state.
pub struct Service {
    rpc_topic: String<MAX_TOPIC_LENGTH>,
    subscribe: Option<Op>,
    ready: bool,
}

impl Service {
    /// Construct an RPC service below one device prefix.
    pub fn new(device_prefix: &str) -> Result<Self, ConfigError> {
        if device_prefix.len() + RPC_SUFFIX.len() + RPC_FILTER_SUFFIX.len() > MAX_TOPIC_LENGTH {
            return Err(ConfigError::TopicTooLong);
        }

        let mut rpc_topic = String::new();
        rpc_topic
            .push_str(device_prefix)
            .map_err(|_| ConfigError::TopicTooLong)?;
        rpc_topic
            .push_str(RPC_SUFFIX)
            .map_err(|_| ConfigError::TopicTooLong)?;

        Ok(Self {
            rpc_topic,
            subscribe: None,
            ready: false,
        })
    }

    /// Begin service startup for a newly connected or resumed MQTT session.
    pub fn begin_connection(&mut self, event: ConnectEvent) {
        self.subscribe = None;
        self.ready = matches!(event, ConnectEvent::Reconnected);
    }

    /// Return whether the request subscription is active.
    pub const fn is_ready(&self) -> bool {
        self.ready
    }

    /// Advance subscription startup without consuming inbound publications.
    ///
    /// Returns `Ok(true)` when the service is ready. The caller must continue driving the MQTT
    /// connection between calls while this returns `Ok(false)`.
    pub async fn step<IO: Io>(
        &mut self,
        connection: &mut Connection<'_, '_, IO>,
    ) -> Result<bool, MqttError<IO::Error>> {
        if self.ready {
            return Ok(true);
        }

        if let Some(op) = self.subscribe {
            if connection.is_pending(&op) {
                return Ok(false);
            }
            if connection.is_complete(&op) {
                self.subscribe = None;
                self.ready = true;
                return Ok(true);
            }
            debug_assert!(connection.is_invalidated(&op));
            self.subscribe = None;
            return Err(MqttError::Disconnected);
        }

        let options = SubscriptionOptions::default()
            .maximum_qos(QoS::AtLeastOnce)
            .retain_behavior(RetainHandling::Never)
            .retain_as_published()
            .ignore_local_messages();
        let mut rpc_filter = self.rpc_topic.clone();
        rpc_filter.push_str(RPC_FILTER_SUFFIX).unwrap();
        match connection
            .subscribe(&[TopicFilter::new(&rpc_filter).options(options)], &[])
            .await
        {
            Ok(op) => self.subscribe = Some(op),
            Err(MqttError::NotReady | MqttError::Resource(ResourceError::InflightExhausted)) => {}
            Err(err) => return Err(err),
        }
        Ok(false)
    }

    /// Classify an inbound publication and retain the response destination for valid requests.
    pub fn handle<'a>(&self, inbound: &'a InboundPublish<'a>) -> Handle<'a> {
        let Some(method) = self.method(inbound.topic()) else {
            return Handle::Unhandled;
        };

        let response = inbound.reply_owned::<MAX_TOPIC_LENGTH, MAX_CORRELATION_LENGTH>();
        if inbound.retained() {
            return Handle::Rejected(Rejected {
                reason: RejectReason::Retained,
                response: response.ok().flatten(),
            });
        }
        let response = match response {
            Ok(response) => response,
            Err(_) => {
                return Handle::Rejected(Rejected {
                    reason: RejectReason::ResponseTargetTooLong,
                    response: None,
                });
            }
        };
        let Some(response) = response else {
            return Handle::Rejected(Rejected {
                reason: RejectReason::MissingResponseTopic,
                response: None,
            });
        };
        if method.is_empty() {
            return Handle::Rejected(Rejected {
                reason: RejectReason::EmptyMethod,
                response: Some(response),
            });
        }

        Handle::Request(Request {
            method,
            payload: inbound.payload(),
            response,
        })
    }

    fn method<'a>(&self, topic: &'a str) -> Option<&'a str> {
        let suffix = topic.strip_prefix(self.rpc_topic.as_str())?;
        if suffix.is_empty() {
            return Some("");
        }
        suffix.strip_prefix('/')
    }
}

/// Publish a transient, correlated RPC response.
///
/// `code` is attached as the [`RESPONSE_CODE_PROPERTY`] MQTT user property. Use
/// [`SUCCESS_CODE`] for success; other values are application-defined failures. Responses use
/// QoS 1, are never retained, and expire after [`RESPONSE_EXPIRY_SECS`]. Payload interpretation
/// and payload-format properties remain application-owned.
pub async fn respond<IO, P>(
    connection: &mut Connection<'_, '_, IO>,
    target: &ResponseTarget,
    code: &str,
    payload: P,
) -> Result<Option<Op>, PubError<P::Error, IO::Error>>
where
    IO: Io,
    P: ToPayload,
{
    let properties = [
        Property::MessageExpiryInterval(RESPONSE_EXPIRY_SECS),
        Property::UserProperty(RESPONSE_CODE_PROPERTY, code),
    ];
    connection
        .publish(
            target
                .publication(payload)
                .properties(&properties)
                .qos(QoS::AtLeastOnce),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joins_prefix() {
        assert_eq!(
            Service::new("device").unwrap().rpc_topic.as_str(),
            "device/rpc"
        );
        assert_eq!(Service::new("").unwrap().rpc_topic.as_str(), "/rpc");
        assert_eq!(
            Service::new("device/").unwrap().rpc_topic.as_str(),
            "device//rpc"
        );
    }

    #[test]
    fn routes_only_the_rpc_tree() {
        let service = Service::new("root/device").unwrap();
        assert_eq!(service.method("root/device/rpc"), Some(""));
        assert_eq!(
            service.method("root/device/rpc/settings/store"),
            Some("settings/store")
        );
        // Empty topic levels are valid MQTT syntax and remain application-visible.
        assert_eq!(
            service.method("root/device/rpc//settings/store"),
            Some("/settings/store")
        );
        assert_eq!(service.method("root/device/rpcx/store"), None);
        assert_eq!(service.method("other/device/rpc/store"), None);
    }

    #[test]
    fn rejects_oversized_topic() {
        let prefix = "x".repeat(MAX_TOPIC_LENGTH);
        assert_eq!(Service::new(&prefix).err(), Some(ConfigError::TopicTooLong));
    }
}
