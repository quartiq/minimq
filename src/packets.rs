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

/// An MQTT CONNECT packet.
#[derive(Debug)]
pub struct Connect<'a, const T: usize> {
    /// Specifies the keep-alive interval of the connection in seconds.
    pub keep_alive: u16,

    /// Any properties associated with the CONNECT request.
    pub properties: Properties<'a>,

    /// The ID of the client that is connecting. May be an empty string to automatically allocate
    /// an ID from the broker.
    pub client_id: Utf8String<'a>,

    /// An optional will message to be transmitted whenever the connection is lost.
    pub will: Option<&'a Will<T>>,

    /// Specified true there is no session state being taken in to the MQTT connection.
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

/// An MQTT CONNACK packet, representing a connection acknowledgement from a broker.
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

/// An MQTT PUBLISH packet, containing data to be sent or received.
#[derive(Debug)]
pub struct Pub<'a> {
    /// The topic that the message was received on.
    pub topic: Utf8String<'a>,

    /// The ID of the internal message.
    pub packet_id: Option<u16>,

    /// The properties transmitted with the publish data.
    pub properties: Vec<Property<'a>, 8>,

    /// The message to be transmitted.
    pub payload: &'a [u8],

    /// Specifies whether or not the message should be retained on the broker.
    pub retain: Retain,

    /// Specifies the quality-of-service of the transmission.
    pub qos: QoS,

    /// Specified true if this message is a duplicate (e.g. it has already been transmitted).
    pub dup: bool,
}

impl<'a> serde::Serialize for Pub<'a> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut item = serializer.serialize_struct("Publish", 0)?;
        item.serialize_field("topic", &self.topic)?;

        // Packet identifiers are absent unless a QoS requiring IDs is specified.
        if self.qos > QoS::AtMostOnce {
            item.serialize_field("packet_identifier", self.packet_id.as_ref().unwrap())?;
        }

        item.serialize_field("properties", &Properties(self.properties.as_slice()))?;
        item.serialize_field("payload", self.payload)?;

        item.end()
    }
}

/// An MQTT SUBSCRIBE control packet
#[derive(Debug, Serialize)]
pub struct Subscribe<'a> {
    /// Specifies the ID of this subscription request.
    pub packet_id: u16,

    /// A list of properties associated with the subscription.
    pub properties: Properties<'a>,

    /// A list of topic filters and associated subscription options for the subscription request.
    pub topics: &'a [(Utf8String<'a>, SubscriptionOptions)],
}

/// An MQTT PINGREQ control packet
#[derive(Debug, Serialize)]
pub struct PingReq;

/// An MQTT PINGRESP control packet
#[derive(Debug, Deserialize)]
pub struct PingResp;

/// An MQTT PUBACK control packet
#[derive(Debug, Deserialize)]
pub struct PubAck<'a> {
    /// The ID of the packet being acknowledged.
    pub packet_identifier: u16,

    /// The properties and reason code associated with the packet.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

/// An MQTT SUBACK control packet.
#[derive(Debug, Deserialize)]
pub struct SubAck<'a> {
    /// The identifier that the acknowledge is assocaited with.
    pub packet_identifier: u16,

    /// The optional properties associated with the acknowledgement.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,

    /// The response status code of the subscription request.
    pub code: u8,
}

/// An MQTT PUBREC control packet
#[derive(Debug, Deserialize)]
pub struct PubRec<'a> {
    /// The ID of the packet that publication reception occurred on.
    pub packet_id: u16,

    /// The properties and success status of associated with the publication.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

/// An MQTT PUBREL control packet
#[derive(Debug, Serialize)]
pub struct PubRel<'a> {
    /// The ID of the publication that this packet is associated with.
    pub packet_id: u16,

    /// The response code of the publish release message.
    pub code: u8,

    /// Properties associated wtih the packet
    pub properties: Properties<'a>,
}

/// An MQTT PUBCOMP control packet
#[derive(Debug, Deserialize)]
pub struct PubComp<'a> {
    /// Packet identifier of the publication that this packet is associated with.
    pub packet_id: u16,

    /// The properties and reason code associated with this packet.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

/// An MQTT DISCONNECT control packet
#[derive(Debug, Deserialize)]
pub struct Disconnect<'a> {
    /// The success status of the disconnection.
    pub reason_code: u8,

    /// Properties associated with the disconnection.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}

/// Success information for a control packet with optional data.
#[derive(Debug, Deserialize)]
pub struct Reason<'a> {
    #[serde(borrow)]
    reason: Option<ReasonData<'a>>,
}

impl<'a> Reason<'a> {
    /// Get the reason code of the packet.
    pub fn code(&self) -> u8 {
        self.reason.as_ref().map(|data| data.code).unwrap_or(0)
    }

    /// Get the properties assocaited with the packet.
    pub fn properties(&self) -> &'_ [Property<'a>] {
        match &self.reason {
            Some(ReasonData { properties, .. }) => properties,
            _ => &[],
        }
    }
}

#[derive(Debug, Deserialize)]
struct ReasonData<'a> {
    /// Reason code
    pub code: u8,

    /// The properties transmitted with the publish data.
    #[serde(borrow)]
    pub properties: Vec<Property<'a>, 8>,
}
