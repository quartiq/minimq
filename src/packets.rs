use crate::{
    properties::Property,
    types::{Properties, SubscriptionOptions, Utf8String},
    will::Will,
    QoS, Retain,
};
use bit_field::BitField;
use heapless::Vec;
use serde::{Deserialize, Serialize};

use serde::ser::SerializeStruct;

#[derive(Debug)]
pub struct Connect<'a, const T: usize> {
    pub keep_alive: u16,
    pub properties: Properties<'a>,
    pub client_id: Utf8String<'a>,
    pub will: Option<&'a Will<T>>,
    pub clean_start: bool,
}

impl<'a, const T: usize> serde::Serialize for Connect<'a, T> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut flags: u8 = 0;
        flags.set_bit(1, self.clean_start);

        if let Some(will) = &self.will {
            // Update the flags for the will parameters. Indicate that the will is present, the QoS of
            // the will message, and whether or not the will message should be retained.
            flags.set_bit(2, true);
            flags.set_bits(3..=4, will.qos as u8);
            flags.set_bit(5, will.retain == Retain::Retained);
        }

        let mut item = serializer.serialize_struct("Connect", 0)?;
        item.serialize_field("protocol_name", &Utf8String("MQTT"))?;
        item.serialize_field("protocol_version", &5u8)?;
        item.serialize_field("flags", &flags)?;
        item.serialize_field("keep_alive", &self.keep_alive)?;
        item.serialize_field("properties", &self.properties)?;
        item.serialize_field("client_id", &self.client_id)?;
        item.serialize_field("will", &self.will)?;

        item.end()
    }
}

#[derive(Debug, Deserialize)]
pub struct ConnAck<'a> {
    /// Indicates true if session state is being maintained by the broker.
    pub session_present: bool,

    /// A status code indicating the success status of the connection.
    pub reason_code: u8,

    /// A list of properties associated with the connection.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug)]
pub struct Pub<'a> {
    /// The topic that the message was received on.
    pub topic: Utf8String<'a>,

    pub packet_id: Option<u16>,

    /// The properties transmitted with the publish data.
    pub properties: Vec<Property<'a>, 8>,

    pub payload: &'a [u8],
    pub retain: Retain,
    pub qos: QoS,
    pub dup: bool,
}

impl<'a> serde::Serialize for Pub<'a> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut item = serializer.serialize_struct("Publish", 0)?;
        item.serialize_field("topic", &self.topic)?;
        if self.qos > QoS::AtMostOnce {
            item.serialize_field("packet_identifier", self.packet_id.as_ref().unwrap())?;
        }
        item.serialize_field("properties", &Properties(self.properties.as_slice()))?;
        item.serialize_field("payload", self.payload)?;

        item.end()
    }
}

#[derive(Debug, Serialize)]
pub struct Subscribe<'a> {
    pub packet_id: u16,
    pub properties: Properties<'a>,
    pub topics: &'a [(Utf8String<'a>, SubscriptionOptions)],
}

#[derive(Debug, Serialize)]
pub struct PingReq;

#[derive(Debug, Deserialize)]
pub struct PingResp;

#[derive(Debug, Deserialize)]
pub struct PubAck<'a> {
    /// Packet identifier
    pub packet_identifier: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Deserialize)]
pub struct SubAck<'a> {
    /// The identifier that the acknowledge is assocaited with.
    pub packet_identifier: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,

    pub code: u8,
}

#[derive(Debug, Deserialize)]
pub struct PubRec<'a> {
    /// Packet identifier
    pub packet_id: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Serialize)]
pub struct PubRel<'a> {
    /// Packet identifier
    pub packet_id: u16,

    pub code: u8,
    pub properties: Properties<'a>,
}

#[derive(Debug, Deserialize)]
pub struct PubComp<'a> {
    /// Packet identifier
    pub packet_id: u16,

    /// The optional properties and reason code.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Deserialize)]
pub struct Disconnect<'a> {
    /// The success status of the disconnection.
    pub reason_code: u8,

    /// Properties associated with the disconnection.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}

#[derive(Debug, Deserialize)]
pub struct Reason<'a> {
    #[serde(borrow)]
    reason: Option<ReasonData<'a>>,
}

impl<'a> Reason<'a> {
    pub fn code(&self) -> u8 {
        self.reason.as_ref().map(|data| data.code).unwrap_or(0)
    }

    pub fn properties(&self) -> &'_ [Property<'a>] {
        match &self.reason {
            Some(ReasonData { properties, .. }) => properties,
            _ => &[],
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ReasonData<'a> {
    /// Reason code
    pub code: u8,

    /// The properties transmitted with the publish data.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}
