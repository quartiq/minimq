//! Internal MQTT wire-format helpers.

use crate::{
    Retain,
    packets::{
        ConnAck, Connect, Disconnect, PingReq, PubAck, PubComp, PubRec, PubRel, PublishHeader,
        SubAck, Subscribe, UnsubAck, Unsubscribe,
    },
};
use num_enum::TryFromPrimitive;
use serde::ser::SerializeStruct;

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

#[derive(Copy, Clone, Debug, TryFromPrimitive)]
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

pub(crate) trait ControlPacket {
    const MESSAGE_TYPE: MessageType;

    fn fixed_header_flags(&self) -> u8 {
        0
    }
}

impl ControlPacket for Connect<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Connect;
}

impl ControlPacket for ConnAck<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::ConnAck;
}

impl PublishHeader<'_> {
    pub(crate) fn fixed_header_flags(&self) -> u8 {
        let mut flags = (self.qos as u8) << 1;
        if self.retain == Retain::Retained {
            flags |= 1;
        }
        if self.dup {
            flags |= 1 << 3;
        }
        flags
    }
}

impl ControlPacket for PubAck<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubAck;
}

impl ControlPacket for PubRec<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubRec;
}

impl ControlPacket for PubRel<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubRel;

    fn fixed_header_flags(&self) -> u8 {
        0b0010
    }
}

impl ControlPacket for PubComp<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::PubComp;
}

impl ControlPacket for Subscribe<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Subscribe;

    fn fixed_header_flags(&self) -> u8 {
        0b0010 | ((self.dup as u8) << 3)
    }
}

impl ControlPacket for SubAck<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::SubAck;
}

impl ControlPacket for Unsubscribe<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Unsubscribe;

    fn fixed_header_flags(&self) -> u8 {
        0b0010 | ((self.dup as u8) << 3)
    }
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
