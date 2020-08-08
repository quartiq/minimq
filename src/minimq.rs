// Minimal MQTT v5.0 client implementation

use crate::mqtt_client::ProtocolError;
use crate::ser::serialize::integer_size;
use crate::ser::PacketWriter;
use enum_iterator::IntoEnumIterator;

use crate::properties;

const CLIENT_ID_MAX: usize = 23;

#[derive(IntoEnumIterator, Copy, Clone, PartialEq, Debug)]
pub enum MessageType {
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
    pub sid: Option<usize>,

    pub topic: Meta,
    pub response: Option<Meta>,
    pub cd: Option<Meta>,
}

impl PubInfo {
    pub fn new() -> PubInfo {
        PubInfo {
            sid: None,
            topic: Meta::new(&[]),
            response: None,
            cd: None,
        }
    }

    pub fn variable_header_length(&self) -> usize {
        // Include the length of the mandatory topic name field in the variable header.
        let mut length = self.topic.get().len() + 2;

        // TODO: Handle sender ID for QoS 1 or 2.

        let property_length = self.get_property_length();

        // Include length of the properties field.
        length += property_length + integer_size(property_length);

        length
    }

    fn get_property_length(&self) -> usize {
        let mut property_length = 0;
        if let Some(response) = &self.response {
            // The length of this entry is 2 bytes for the string length encoding and the string
            // data.
            property_length += integer_size(properties::RESPONSE_TOPIC);
            property_length += response.get().len() + 2;
        }

        if let Some(cd) = &self.cd {
            property_length += integer_size(properties::CORRELATION_DATA);
            property_length += cd.get().len() + 2;
        }

        property_length
    }

    pub fn write_variable_header(&self, packet: &mut PacketWriter) -> Result<(), ProtocolError> {
        // Write the topic name.
        packet.write_binary_data(self.topic.get())?;

        // TODO: Handle the sender ID.

        // Write the length of the properties list.
        packet.write_variable_length_integer(self.get_property_length())?;

        // Write the response topic property.
        if let Some(meta) = &self.response {
            packet.write_variable_length_integer(properties::RESPONSE_TOPIC)?;
            packet.write_binary_data(meta.get())?;
        }

        // Write the correlation data.
        if let Some(meta) = &self.cd {
            packet.write_variable_length_integer(properties::CORRELATION_DATA)?;
            packet.write_binary_data(meta.get())?;
        }

        // TODO: Handle additional properties.

        Ok(())
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

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn property() {
        let data = properties::property_data(properties::SUBSCRIPTION_IDENTIFIER);
        assert_eq!(data.unwrap(), properties::Data::VariableByteInteger);
        let none = properties::property_data(0);
        assert_eq!(none, None);
    }
}
