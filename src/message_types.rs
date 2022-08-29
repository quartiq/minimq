use crate::{
    packets::{
        ConnAck, Connect, Disconnect, PingReq, PingResp, Pub, PubAck, PubComp, PubRec, PubRel,
        SubAck, Subscribe,
    },
    Retain,
};
use bit_field::BitField;
use num_enum::TryFromPrimitive;

#[derive(Copy, Clone, Debug, TryFromPrimitive)]
#[repr(u8)]
pub enum MessageType {
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

pub trait ControlPacket {
    const MESSAGE_TYPE: MessageType;
    fn fixed_header_flags(&self) -> u8 {
        0u8
    }
}

impl<'a, const T: usize> ControlPacket for Connect<'a, T> {
    const MESSAGE_TYPE: MessageType = MessageType::Connect;
}

impl<'a> ControlPacket for ConnAck<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::ConnAck;
}

impl<'a> ControlPacket for Pub<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::Publish;
    fn fixed_header_flags(&self) -> u8 {
        *0u8.set_bits(1..=2, self.qos as u8)
            .set_bit(0, self.retain == Retain::Retained)
    }
}

impl<'a> ControlPacket for PubAck<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::PubAck;
}

impl<'a> ControlPacket for PubRec<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::PubRec;
}

impl<'a> ControlPacket for PubRel<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::PubRel;
    fn fixed_header_flags(&self) -> u8 {
        0b0010
    }
}

impl<'a> ControlPacket for PubComp<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::PubRec;
}

impl<'a> ControlPacket for Subscribe<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::Subscribe;
    fn fixed_header_flags(&self) -> u8 {
        0b0010
    }
}

impl<'a> ControlPacket for SubAck<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::SubAck;
}

impl ControlPacket for PingReq {
    const MESSAGE_TYPE: MessageType = MessageType::PingReq;
}

impl ControlPacket for PingResp {
    const MESSAGE_TYPE: MessageType = MessageType::PingResp;
}

impl<'a> ControlPacket for Disconnect<'a> {
    const MESSAGE_TYPE: MessageType = MessageType::Disconnect;
}
