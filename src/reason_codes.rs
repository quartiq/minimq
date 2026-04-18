use crate::ProtocolError;
use num_enum::{FromPrimitive, IntoPrimitive};

/// MQTTv5-defined codes that may be returned in response to control packets.
#[derive(PartialEq, PartialOrd, Copy, Clone, Debug, FromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum ReasonCode {
    /// Success.
    Success = 0x00,
    /// Granted QoS 1.
    GrantedQos1 = 0x01,
    /// Granted QoS 2.
    GrantedQos2 = 0x02,
    /// Disconnect and publish the will.
    DisconnectWithWill = 0x04,
    /// The publish matched no subscribers.
    NoMatchingSubscribers = 0x10,
    /// The unsubscribe found no existing subscription.
    NoSubscriptionExisted = 0x11,
    /// Continue authentication exchange.
    ContinueAuthentication = 0x18,
    /// Re-authentication is requested.
    Reauthenticate = 0x19,

    /// Unspecified error.
    UnspecifiedError = 0x80,
    /// Malformed packet.
    MalformedPacket = 0x81,
    /// Protocol error.
    ProtocolError = 0x82,
    /// Implementation-specific error.
    ImplementationError = 0x83,
    /// Unsupported protocol version.
    UnsupportedProtocol = 0x84,
    /// Invalid client identifier.
    ClientIdentifierInvalid = 0x85,
    /// Bad username or password.
    BadUsernameOrPassword = 0x86,
    /// Not authorized.
    NotAuthorized = 0x87,
    /// Server unavailable.
    ServerUnavailable = 0x88,
    /// Server busy.
    ServerBusy = 0x89,
    /// Bad authentication method.
    BadAuthMethod = 0x8c,
    /// Keepalive timeout.
    KeepAliveTimeout = 0x8d,
    /// Session taken over by another client.
    SessionTakenOver = 0x8e,
    /// Invalid topic filter.
    TopicFilterInvalid = 0x8f,
    /// Invalid topic name.
    TopicNameInvalid = 0x90,
    /// Packet identifier already in use.
    PacketIdInUse = 0x91,
    /// Packet identifier not found.
    PacketIdNotFound = 0x92,
    /// Receive maximum exceeded.
    ReceiveMaxExceeded = 0x93,
    /// Invalid topic alias.
    TopicAliasInvalid = 0x94,
    /// Packet too large.
    PacketTooLarge = 0x95,
    /// Message rate too high.
    MessageRateTooHigh = 0x96,
    /// Quota exceeded.
    QuotaExceeded = 0x97,
    /// Administrative action.
    AdministrativeAction = 0x98,
    /// Payload format invalid.
    PayloadFormatInvalid = 0x99,
    /// Retained messages are not supported.
    RetainNotSupported = 0x9A,
    /// QoS is not supported.
    QoSNotSupported = 0x9b,
    /// Use another server.
    UseAnotherServer = 0x9c,
    /// Server moved.
    ServerMoved = 0x9d,
    /// Shared subscriptions are not supported.
    SharedSubscriptionsNotSupported = 0x9e,
    /// Connection rate exceeded.
    ConnectionRateExceeded = 0x9f,
    /// Maximum connect time reached.
    MaximumConnectTime = 0xa0,
    /// Subscription identifiers are not supported.
    SubscriptionIdentifiersNotSupported = 0xa1,
    /// Wildcard subscriptions are not supported.
    WildcardSubscriptionsNotSupported = 0xa2,

    /// The reason code is not one of the documented MQTT reason codes.
    #[num_enum(default)]
    Unknown = 0xFF,
}

impl serde::Serialize for ReasonCode {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.into())
    }
}

struct ReasonCodeVisitor;

impl serde::de::Visitor<'_> for ReasonCodeVisitor {
    type Value = ReasonCode;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "ReasonCode")
    }

    fn visit_u8<E: serde::de::Error>(self, data: u8) -> Result<Self::Value, E> {
        Ok(ReasonCode::from(data))
    }
}

impl<'de> serde::de::Deserialize<'de> for ReasonCode {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_u8(ReasonCodeVisitor)
    }
}

impl From<&ReasonCode> for u8 {
    fn from(code: &ReasonCode) -> u8 {
        (*code).into()
    }
}

impl ReasonCode {
    /// Convert the reason code to `Ok(())` for success codes or a protocol error otherwise.
    pub fn as_result(&self) -> Result<(), ProtocolError> {
        if self.success() {
            return Ok(());
        }
        Err(ProtocolError::Failed(*self))
    }

    /// Return `true` for MQTT success codes.
    pub fn success(&self) -> bool {
        let value: u8 = self.into();
        value < 0x80
    }

    /// Return `true` for MQTT failure codes.
    pub fn failed(&self) -> bool {
        !self.success()
    }
}
