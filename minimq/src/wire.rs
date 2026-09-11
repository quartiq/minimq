//! Internal MQTT wire-format helpers.

use crate::{
    QoS, Retain,
    packets::{
        ConnAck, Connect, Disconnect, PingReq, PubAck, PubComp, PubRec, PubRel, PublishHeader,
        SubAck, Subscribe, UnsubAck, Unsubscribe,
    },
};
use num_enum::TryFromPrimitive;
use serde::ser::SerializeStruct;

const FIXED_HEADER_TYPE_SHIFT: u32 = 4;
const FIXED_HEADER_FLAGS_MASK: u8 = (1 << FIXED_HEADER_TYPE_SHIFT) - 1;
const PUBLISH_RETAIN_FLAG: u8 = 1 << 0;
const PUBLISH_QOS_SHIFT: u8 = 1;
const PUBLISH_QOS_MASK: u8 = 0b11 << PUBLISH_QOS_SHIFT;
const PUBLISH_DUP_FLAG: u8 = 1 << 3;
const REQUIRED_CONTROL_FLAGS: u8 = 0b0010;

const CONNECT_CLEAN_START_FLAG: u8 = 1 << 1;
const CONNECT_WILL_FLAG: u8 = 1 << 2;
const CONNECT_WILL_QOS_SHIFT: u8 = 3;
const CONNECT_WILL_RETAIN_FLAG: u8 = 1 << 5;
const CONNECT_PASSWORD_FLAG: u8 = 1 << 6;
const CONNECT_USER_NAME_FLAG: u8 = 1 << 7;

/// MQTT binary data field.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct BinaryData<'a>(pub(crate) &'a [u8]);

impl serde::Serialize for BinaryData<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let len = u16::try_from(self.0.len())
            .map_err(|_| S::Error::custom("Provided binary data is too long"))?;
        let mut item = serializer.serialize_struct("_BinaryData", 0)?;
        item.serialize_field("_len", &len)?;
        item.serialize_field("_data", self.0)?;
        item.end()
    }
}

struct BinaryDataVisitor;

impl<'de> serde::de::Visitor<'de> for BinaryDataVisitor {
    type Value = BinaryData<'de>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "BinaryData")
    }

    fn visit_borrowed_bytes<E: serde::de::Error>(self, data: &'de [u8]) -> Result<Self::Value, E> {
        Ok(BinaryData(data))
    }
}

impl<'de> serde::de::Deserialize<'de> for BinaryData<'de> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_bytes(BinaryDataVisitor)
    }
}

/// MQTT UTF-8 string field.
#[derive(Copy, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub(crate) struct Utf8String<'a>(pub(crate) &'a str);

impl serde::Serialize for Utf8String<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;

        let len = u16::try_from(self.0.len())
            .map_err(|_| S::Error::custom("Provided string is too long"))?;
        let mut item = serializer.serialize_struct("_Utf8String", 0)?;
        item.serialize_field("_len", &len)?;
        item.serialize_field("_string", self.0)?;
        item.end()
    }
}

struct Utf8StringVisitor<'a> {
    _data: core::marker::PhantomData<&'a ()>,
}

impl<'a, 'de: 'a> serde::de::Visitor<'de> for Utf8StringVisitor<'a> {
    type Value = Utf8String<'a>;

    fn expecting(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(formatter, "Utf8String")
    }

    fn visit_borrowed_str<E: serde::de::Error>(self, data: &'de str) -> Result<Self::Value, E> {
        Ok(Utf8String(data))
    }
}

impl<'a, 'de: 'a> serde::de::Deserialize<'de> for Utf8String<'a> {
    fn deserialize<D: serde::de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_str(Utf8StringVisitor {
            _data: core::marker::PhantomData,
        })
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, TryFromPrimitive)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[repr(u8)]
pub(crate) enum MessageType {
    Connect = 1,
    ConnAck = 2,
    Publish = 3,
    PubAck = 4,
    PubRec = 5,
    PubRel = 6,
    PubComp = 7,
    Subscribe = 8,
    SubAck = 9,
    Unsubscribe = 10,
    UnsubAck = 11,
    PingReq = 12,
    PingResp = 13,
    Disconnect = 14,
    Auth = 15,
}

impl MessageType {
    const fn required_flags(self) -> u8 {
        match self {
            Self::PubRel | Self::Subscribe | Self::Unsubscribe => REQUIRED_CONTROL_FLAGS,
            _ => 0,
        }
    }

    const fn flags_valid(self, flags: u8) -> bool {
        matches!(self, Self::Publish) || flags == self.required_flags()
    }
}

#[derive(Copy, Clone)]
pub(crate) struct FixedHeader(u8);

impl FixedHeader {
    const fn new(message_type: MessageType, flags: u8) -> Self {
        debug_assert!(flags <= FIXED_HEADER_FLAGS_MASK);
        Self(((message_type as u8) << FIXED_HEADER_TYPE_SHIFT) | flags)
    }

    pub(crate) const fn from_byte(byte: u8) -> Self {
        Self(byte)
    }

    pub(crate) const fn byte(self) -> u8 {
        self.0
    }

    pub(crate) fn message_type(self) -> Option<MessageType> {
        MessageType::try_from(self.0 >> FIXED_HEADER_TYPE_SHIFT).ok()
    }

    pub(crate) const fn flags(self) -> u8 {
        self.0 & FIXED_HEADER_FLAGS_MASK
    }

    pub(crate) fn flags_valid(self) -> bool {
        self.message_type()
            .is_some_and(|message_type| message_type.flags_valid(self.flags()))
    }

    pub(crate) fn publish_qos(self) -> Option<QoS> {
        QoS::try_from((self.flags() & PUBLISH_QOS_MASK) >> PUBLISH_QOS_SHIFT).ok()
    }

    pub(crate) const fn publish_retain(self) -> Retain {
        if self.flags() & PUBLISH_RETAIN_FLAG == 0 {
            Retain::NotRetained
        } else {
            Retain::Retained
        }
    }

    pub(crate) const fn publish_duplicate(self) -> bool {
        self.flags() & PUBLISH_DUP_FLAG != 0
    }

    const fn with_publish_duplicate(self) -> Self {
        Self(self.0 | PUBLISH_DUP_FLAG)
    }
}

pub(crate) trait ControlPacket {
    const MESSAGE_TYPE: MessageType;

    fn fixed_header(&self) -> FixedHeader {
        FixedHeader::new(Self::MESSAGE_TYPE, Self::MESSAGE_TYPE.required_flags())
    }
}

impl ControlPacket for Connect<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Connect;
}

impl ControlPacket for ConnAck<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::ConnAck;
}

impl PublishHeader<'_> {
    pub(crate) fn fixed_header(&self) -> FixedHeader {
        let mut flags = (self.qos as u8) << PUBLISH_QOS_SHIFT;
        if self.retain == Retain::Retained {
            flags |= PUBLISH_RETAIN_FLAG;
        }
        if self.dup {
            flags |= PUBLISH_DUP_FLAG;
        }
        FixedHeader::new(MessageType::Publish, flags)
    }
}

impl Connect<'_> {
    pub(crate) fn flags(&self) -> u8 {
        let mut flags = 0;
        if self.clean_start {
            flags |= CONNECT_CLEAN_START_FLAG;
        }
        if let Some(will) = &self.will {
            flags |= CONNECT_WILL_FLAG | ((will.qos_level() as u8) << CONNECT_WILL_QOS_SHIFT);
            if will.retained_flag() == Retain::Retained {
                flags |= CONNECT_WILL_RETAIN_FLAG;
            }
        }
        if self.auth.is_some() {
            flags |= CONNECT_USER_NAME_FLAG | CONNECT_PASSWORD_FLAG;
        }
        flags
    }
}

/// Mark an encoded PUBLISH fixed header as a retransmission.
pub(crate) fn mark_publish_duplicate(fixed_header: &mut u8) {
    let header = FixedHeader::from_byte(*fixed_header);
    debug_assert_eq!(header.message_type(), Some(MessageType::Publish));
    *fixed_header = header.with_publish_duplicate().byte();
}

impl ControlPacket for PubAck<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubAck;
}

impl ControlPacket for PubRec<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubRec;
}

impl ControlPacket for PubRel<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubRel;
}

impl ControlPacket for PubComp<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubComp;
}

impl ControlPacket for Subscribe<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Subscribe;
}

impl ControlPacket for SubAck<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::SubAck;
}

impl ControlPacket for Unsubscribe<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Unsubscribe;
}

impl ControlPacket for UnsubAck<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::UnsubAck;
}

impl ControlPacket for PingReq {
    const MESSAGE_TYPE: MessageType = MessageType::PingReq;
}

impl ControlPacket for Disconnect<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Disconnect;
}
