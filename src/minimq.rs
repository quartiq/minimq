// Minimal MQTT v5.0 client implementation

use enum_iterator::IntoEnumIterator;

const CLIENT_ID_MAX: usize = 23;

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

#[derive(Debug)]
pub struct PubInfo {
    pub topic: Meta,
}

impl PubInfo {
    pub fn new() -> PubInfo {
        PubInfo {
            topic: Meta::new(&[]),
        }
    }
}

const META_MAX: usize = 64;

impl core::fmt::Debug for Meta {
    fn fmt(&self, formatter: &mut core::fmt::Formatter) -> core::fmt::Result {
        self.buf[..].fmt(formatter)?;
        self.len.fmt(formatter)
    }
}

#[derive(Clone, Copy)]
pub struct Meta {
    pub buf: [u8; META_MAX],
    pub len: usize,
}

impl Meta {
    pub fn new(data: &[u8]) -> Meta {
        let mut meta = Meta {
            buf: [0; META_MAX],
            len: data.len(),
        };
        meta.set(data).unwrap();
        meta
    }

    pub fn get(&self) -> &[u8] {
        &self.buf[..self.len]
    }

    pub fn set(&mut self, data: &[u8]) -> Result<(), ()> {
        if data.len() <= META_MAX {
            self.len = data.len();
            self.buf[..self.len].copy_from_slice(data);
            Ok(())
        } else {
            Err(())
        }
    }
}
