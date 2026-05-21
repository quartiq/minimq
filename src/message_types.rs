use crate::{
    Retain,
    packets::{
        ConnAck, Connect, Disconnect, PingReq, PingResp, PubAck, PubComp, PubRec, PubRel,
        PublishHeader, SubAck, Subscribe, UnsubAck, Unsubscribe,
    },
};
use num_enum::TryFromPrimitive;

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
        0u8
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

impl ControlPacket for PingResp {
    const MESSAGE_TYPE: MessageType = MessageType::PingResp;
}

impl ControlPacket for Disconnect<'_> {
    const MESSAGE_TYPE: MessageType = MessageType::Disconnect;
}
