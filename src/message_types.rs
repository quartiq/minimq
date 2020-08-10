use enum_iterator::IntoEnumIterator;

#[derive(IntoEnumIterator, Copy, Clone, Debug)]
pub(crate) enum MessageType {
    Invalid = -1,

    Reserved = 0,
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

impl From<u8> for MessageType {
    fn from(val: u8) -> Self {
        for entry in Self::into_enum_iter() {
            if entry as u8 == val {
                return entry;
            }
        }

        return MessageType::Invalid;
    }
}
