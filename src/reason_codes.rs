use crate::ProtocolError;
use num_enum::{FromPrimitive, IntoPrimitive};

/// MQTTv5-defined codes that may be returned in response to control packets.
#[derive(PartialEq, PartialOrd, Copy, Clone, Debug, FromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum ReasonCode {
    Success = 0x00,
    GrantedQos1 = 0x01,
    GrantedQos2 = 0x02,
    DisconnectWithWill = 0x04,
    NoMatchingSubscribers = 0x10,
    NoSubscriptionExisted = 0x11,
    ContinueAuthentication = 0x18,
    Reuathenticate = 0x19,

    UnspecifiedError = 0x80,
    MalformedPacket = 0x81,
    ProtocolError = 0x82,
    ImplementationError = 0x83,
    UnsupportedProtocol = 0x84,
    ClientIdentifierInvalid = 0x85,
    BadUsernameOrPassword = 0x86,
    NotAuthorized = 0x87,
    ServerUnavailable = 0x88,
    ServerBusy = 0x89,
    BadAuthMethod = 0x8c,
    KeepAliveTimeout = 0x8d,
    SessionTakenOver = 0x8e,
    TopicFilterInvalid = 0x8f,
    TopicNameInvalid = 0x90,
    PacketIdInUse = 0x91,
    PacketIdNotFound = 0x92,
    ReceiveMaxExceeded = 0x93,
    TopicAliasINvalid = 0x94,
    PacketTooLarge = 0x95,
    MessageRateTooHigh = 0x96,
    QuotaExceeded = 0x97,
    AdministrativeAction = 0x98,
    PayloadFormatInvalid = 0x99,
    RetainNotSupported = 0x9A,
    QosNotSupported = 0x9b,
    UseAnotherServer = 0x9c,
    ServerMoved = 0x9d,
    SharedSubscriptionsNotSupported = 0x9e,
    ConnectionRateExceeded = 0x9f,
    MaximumConnectTime = 0xa0,
    SubscriptionIdentifiersNotSupported = 0xa1,
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

impl<T, E: Into<ReasonCode>> From<Result<T, E>> for ReasonCode {
    fn from(result: Result<T, E>) -> ReasonCode {
        match result {
            Ok(_) => ReasonCode::Success,
            Err(code) => code.into(),
        }
    }
}

impl From<ReasonCode> for Result<(), ReasonCode> {
    fn from(code: ReasonCode) -> Result<(), ReasonCode> {
        if code.success() {
            Ok(())
        } else {
            Err(code)
        }
    }
}

impl ReasonCode {
    pub fn as_result(&self) -> Result<(), ProtocolError> {
        let result: Result<(), ReasonCode> = (*self).into();
        result?;
        Ok(())
    }

    pub fn success(&self) -> bool {
        let value: u8 = self.into();
        value < 0x80
    }

    pub fn failed(&self) -> bool {
        !self.success()
    }
}
