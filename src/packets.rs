use crate::{
    reason_codes::ReasonCode,
    types::{Auth, Properties, TopicFilter, Utf8String},
    will::SerializedWill,
    QoS, Retain,
};
use bit_field::BitField;
use serde::{Deserialize, Serialize};

use serde::ser::SerializeStruct;

/// An MQTT CONNECT packet.
#[derive(Debug)]
pub struct Connect<'a> {
    /// Specifies the keep-alive interval of the connection in seconds.
    pub keep_alive: u16,

    /// Any properties associated with the CONNECT request.
    pub properties: Properties<'a>,

    /// The ID of the client that is connecting. May be an empty string to automatically allocate
    /// an ID from the broker.
    pub client_id: Utf8String<'a>,

    /// An optional authentication message used by the server.
    pub auth: Option<Auth<'a>>,

    /// An optional will message to be transmitted whenever the connection is lost.
    pub(crate) will: Option<SerializedWill<'a>>,

    /// Specified true there is no session state being taken in to the MQTT connection.
    pub clean_start: bool,
}

impl<'a> serde::Serialize for Connect<'a> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut flags: u8 = 0;
        flags.set_bit(1, self.clean_start);

        if let Some(will) = &self.will {
            // Update the flags for the will parameters. Indicate that the will is present, the QoS of
            // the will message, and whether or not the will message should be retained.
            flags.set_bit(2, true);
            flags.set_bits(3..=4, will.qos as u8);
            flags.set_bit(5, will.retained == Retain::Retained);
        }

        #[cfg(feature = "unsecure")]
        if self.auth.is_some() {
            flags.set_bit(6, true);
            flags.set_bit(7, true);
        }

        let mut item = serializer.serialize_struct("Connect", 0)?;
        item.serialize_field("protocol_name", &Utf8String("MQTT"))?;
        item.serialize_field("protocol_version", &5u8)?;
        item.serialize_field("flags", &flags)?;
        item.serialize_field("keep_alive", &self.keep_alive)?;
        item.serialize_field("properties", &self.properties)?;
        item.serialize_field("client_id", &self.client_id)?;
        if let Some(will) = &self.will {
            item.serialize_field("will", will.contents)?;
        }

        #[cfg(feature = "unsecure")]
        if let Some(auth) = &self.auth {
            item.serialize_field("user_name", &Utf8String(auth.user_name))?;
            item.serialize_field("password", &Utf8String(auth.password))?;
        }

        item.end()
    }
}

/// An MQTT CONNACK packet, representing a connection acknowledgement from a broker.
#[derive(Debug, Deserialize)]
pub struct ConnAck<'a> {
    /// Indicates true if session state is being maintained by the broker.
    pub session_present: bool,

    /// A status code indicating the success status of the connection.
    pub reason_code: ReasonCode,

    /// A list of properties associated with the connection.
    #[serde(borrow)]
    pub properties: Properties<'a>,
}

/// An MQTT PUBLISH packet, containing data to be sent or received.
#[derive(Serialize)]
pub struct Pub<'a, P: crate::publication::ToPayload> {
    /// The topic that the message was received on.
    pub topic: Utf8String<'a>,

    /// The ID of the internal message.
    pub packet_id: Option<u16>,

    /// The properties transmitted with the publish data.
    pub properties: Properties<'a>,

    /// The message to be transmitted.
    #[serde(skip)]
    pub payload: P,

    /// Specifies whether or not the message should be retained on the broker.
    #[serde(skip)]
    pub retain: Retain,

    /// Specifies the quality-of-service of the transmission.
    #[serde(skip)]
    pub qos: QoS,

    /// Specified true if this message is a duplicate (e.g. it has already been transmitted).
    #[serde(skip)]
    pub dup: bool,
}

impl<'a, P: crate::publication::ToPayload> core::fmt::Debug for Pub<'a, P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pub")
            .field("topic", &self.topic)
            .field("packet_id", &self.packet_id)
            .field("properties", &self.properties)
            .field("retain", &self.retain)
            .field("qos", &self.qos)
            .field("dup", &self.dup)
            .field("payload", &"<deferred>")
            .finish()
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
    pub topics: &'a [TopicFilter<'a>],
}

/// An MQTT PINGREQ control packet
#[derive(Debug, Serialize)]
pub struct PingReq;

/// An MQTT PINGRESP control packet
#[derive(Debug, Deserialize)]
pub struct PingResp;

/// An MQTT PUBACK control packet
#[derive(Debug, Deserialize, Serialize)]
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
    pub properties: Properties<'a>,

    /// The response status code of the subscription request.
    #[serde(skip)]
    pub codes: &'a [u8],
}

/// An MQTT PUBREC control packet
#[derive(Debug, Serialize, Deserialize)]
pub struct PubRec<'a> {
    /// The ID of the packet that publication reception occurred on.
    pub packet_id: u16,

    /// The properties and success status of associated with the publication.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

/// An MQTT PUBREL control packet
#[derive(Debug, Deserialize, Serialize)]
pub struct PubRel<'a> {
    /// The ID of the publication that this packet is associated with.
    pub packet_id: u16,

    /// The properties and success status of associated with the publication.
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

/// An MQTT PUBCOMP control packet
#[derive(Debug, Serialize, Deserialize)]
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
    pub reason_code: ReasonCode,

    /// Properties associated with the disconnection.
    #[serde(borrow)]
    pub properties: Properties<'a>,
}

/// Success information for a control packet with optional data.
#[derive(Debug, Deserialize, Serialize)]
pub struct Reason<'a> {
    #[serde(borrow)]
    reason: Option<ReasonData<'a>>,
}

impl<'a> From<ReasonCode> for Reason<'a> {
    fn from(code: ReasonCode) -> Self {
        Self {
            reason: Some(ReasonData {
                code,
                _properties: Some(Properties::Slice(&[])),
            }),
        }
    }
}

impl<'a> Reason<'a> {
    /// Get the reason code of the packet.
    pub fn code(&self) -> ReasonCode {
        self.reason
            .as_ref()
            .map(|data| data.code)
            .unwrap_or(ReasonCode::Success)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ReasonData<'a> {
    /// Reason code
    pub code: ReasonCode,

    /// The properties transmitted with the publish data.
    #[serde(borrow)]
    pub _properties: Option<Properties<'a>>,
}

#[cfg(test)]
mod tests {
    use crate::reason_codes::ReasonCode;
    use crate::ser::MqttSerializer;

    #[test]
    pub fn serialize_publish() {
        let good_publish: [u8; 10] = [
            0x30, // Publish message
            0x08, // Remaining length (8)
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
            0x00, // Properties length
            0xAB, 0xCD, // Payload
        ];

        let publish = crate::packets::Pub {
            qos: crate::QoS::AtMostOnce,
            packet_id: None,
            dup: false,
            properties: crate::types::Properties::Slice(&[]),
            retain: crate::Retain::NotRetained,
            topic: crate::types::Utf8String("ABC"),
            payload: &[0xAB, 0xCD],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::pub_to_buffer(&mut buffer, &publish).unwrap();

        assert_eq!(message, good_publish);
    }

    #[test]
    pub fn serialize_publish_qos1() {
        let good_publish: [u8; 12] = [
            0x32, // Publish message
            0x0a, // Remaining length (10)
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
            0xBE, 0xEF, // Packet identifier
            0x00, // Properties length
            0xAB, 0xCD, // Payload
        ];

        let publish = crate::packets::Pub {
            qos: crate::QoS::AtLeastOnce,
            topic: crate::types::Utf8String("ABC"),
            packet_id: Some(0xBEEF),
            dup: false,
            properties: crate::types::Properties::Slice(&[]),
            retain: crate::Retain::NotRetained,
            payload: &[0xAB, 0xCD],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::pub_to_buffer(&mut buffer, &publish).unwrap();

        assert_eq!(message, good_publish);
    }

    #[test]
    fn serialize_subscribe() {
        let good_subscribe: [u8; 11] = [
            0x82, // Subscribe request
            0x09, // Remaining length (11)
            0x00, 0x10, // Packet identifier (16)
            0x00, // Property length
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
            0x00, // Options byte = 0
        ];

        let subscribe = crate::packets::Subscribe {
            packet_id: 16,
            properties: crate::types::Properties::Slice(&[]),
            topics: &["ABC".into()],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::to_buffer(&mut buffer, &subscribe).unwrap();

        assert_eq!(message, good_subscribe);
    }

    #[test]
    pub fn serialize_publish_with_properties() {
        let good_publish: [u8; 14] = [
            0x30, // Publish message
            0x0c, // Remaining length (14)
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
            0x04, // Properties length - 1 property encoding a string of length 4
            0x08, 0x00, 0x01, 0x41, // Response topic "A"
            0xAB, 0xCD, // Payload
        ];

        let publish = crate::packets::Pub {
            qos: crate::QoS::AtMostOnce,
            topic: crate::types::Utf8String("ABC"),
            packet_id: None,
            dup: false,
            properties: crate::types::Properties::Slice(&[
                crate::properties::Property::ResponseTopic(crate::types::Utf8String("A")),
            ]),
            retain: crate::Retain::NotRetained,
            payload: &[0xAB, 0xCD],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::pub_to_buffer(&mut buffer, &publish).unwrap();

        assert_eq!(message, good_publish);
    }

    #[test]
    pub fn serialize_publish_with_user_prop() {
        let good_publish: [u8; 17] = [
            0x30, // Publish message
            0x0f, // Remaining length (15)
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
            0x07, // Properties length - 1 property encoding a string pair, each of length 1
            0x26, 0x00, 0x01, 0x41, 0x00, 0x01, 0x42, // UserProperty("A", "B")
            0xAB, 0xCD, // Payload
        ];

        let publish = crate::packets::Pub {
            qos: crate::QoS::AtMostOnce,
            topic: crate::types::Utf8String("ABC"),
            packet_id: None,
            dup: false,
            properties: crate::types::Properties::Slice(&[
                crate::properties::Property::UserProperty(
                    crate::types::Utf8String("A"),
                    crate::types::Utf8String("B"),
                ),
            ]),
            retain: crate::Retain::NotRetained,
            payload: &[0xAB, 0xCD],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::pub_to_buffer(&mut buffer, &publish).unwrap();

        assert_eq!(message, good_publish);
    }

    #[test]
    fn serialize_connect() {
        let good_serialized_connect: [u8; 18] = [
            0x10, // Connect
            0x10, // Remaining length (16)
            0x00, 0x04, 0x4d, 0x51, 0x54, 0x54, 0x05, // MQTT5 header
            0x02, // Flags (Clean start)
            0x00, 0x0a, // Keep-alive (10)
            0x00, // Properties length
            0x00, 0x03, 0x41, 0x42, 0x43, // ABC client ID
        ];

        let mut buffer: [u8; 900] = [0; 900];
        let connect: crate::packets::Connect<'_> = crate::packets::Connect {
            client_id: crate::types::Utf8String("ABC"),
            auth: None,
            will: None,
            keep_alive: 10,
            properties: crate::types::Properties::Slice(&[]),
            clean_start: true,
        };

        let message = MqttSerializer::to_buffer(&mut buffer, &connect).unwrap();

        assert_eq!(message, good_serialized_connect)
    }

    #[test]
    fn serialize_connect_with_will() {
        #[rustfmt::skip]
        let good_serialized_connect: [u8; 28] = [
            0x10, // Connect
            26, // Remaining length
    
            // Header: "MQTT5"
            0x00, 0x04, 0x4d, 0x51, 0x54, 0x54, 0x05,
    
            // Flags: Clean start, will present, will not retained, will QoS = 0,
            0b0000_0110,
    
            // Keep-alive: 10 seconds
            0x00, 0x0a,
    
            // Connected Properties: None
            0x00,
            // Client ID: "ABC"
            0x00, 0x03, 0x41, 0x42, 0x43,
            // Will properties: None
            0x00,
            // Will topic: "EFG"
            0x00, 0x03, 0x45, 0x46, 0x47,
            // Will payload: [0xAB, 0xCD]
            0x00, 0x02, 0xAB, 0xCD,
        ];

        let mut buffer: [u8; 900] = [0; 900];
        let mut will_buff = [0; 64];
        let will = crate::will::Will::new("EFG", &[0xAB, 0xCD], &[])
            .unwrap()
            .qos(crate::QoS::AtMostOnce);

        let connect = crate::packets::Connect {
            clean_start: true,
            keep_alive: 10,
            properties: crate::types::Properties::Slice(&[]),
            client_id: crate::types::Utf8String("ABC"),
            auth: None,
            will: Some(will.serialize(&mut will_buff).unwrap()),
        };

        let message = MqttSerializer::to_buffer(&mut buffer, &connect).unwrap();

        assert_eq!(message, good_serialized_connect)
    }

    #[test]
    fn serialize_ping_req() {
        let good_ping_req: [u8; 2] = [
            0xc0, // Ping
            0x00, // Remaining length (0)
        ];

        let mut buffer: [u8; 1024] = [0; 1024];
        let message = MqttSerializer::to_buffer(&mut buffer, &crate::packets::PingReq {}).unwrap();
        assert_eq!(message, good_ping_req);
    }

    #[test]
    fn serialize_pubrel() {
        let good_pubrel: [u8; 6] = [
            6 << 4 | 0b10, // PubRel
            0x04,          // Remaining length
            0x00,
            0x05, // Identifier
            0x10, // Response Code
            0x00, // Properties length
        ];

        let pubrel = crate::packets::PubRel {
            packet_id: 5,
            reason: ReasonCode::NoMatchingSubscribers.into(),
        };

        let mut buffer: [u8; 1024] = [0; 1024];
        assert_eq!(
            MqttSerializer::to_buffer(&mut buffer, &pubrel).unwrap(),
            good_pubrel
        );
    }

    #[test]
    fn serialize_puback() {
        let good_puback: [u8; 5] = [
            4 << 4, // PubAck
            0x03,   // Remaining length
            0x00,
            0x15, // Identifier
            0x00, // Response Code
        ];

        let pubrel = crate::packets::PubAck {
            packet_identifier: 0x15,
            reason: crate::packets::Reason {
                reason: Some(crate::packets::ReasonData {
                    code: ReasonCode::Success,
                    _properties: None,
                }),
            },
        };

        let mut buffer: [u8; 1024] = [0; 1024];
        assert_eq!(
            MqttSerializer::to_buffer(&mut buffer, &pubrel).unwrap(),
            good_puback
        );
    }

    #[test]
    fn serialize_pubcomp() {
        let good_pubcomp: [u8; 5] = [
            7 << 4, // PubComp
            0x03,   // Remaining length
            0x00,
            0x15, // Identifier
            0x00, // Response Code
        ];

        let pubrel = crate::packets::PubComp {
            packet_id: 0x15,
            reason: crate::packets::Reason {
                reason: Some(crate::packets::ReasonData {
                    code: ReasonCode::Success,
                    _properties: None,
                }),
            },
        };

        let mut buffer: [u8; 1024] = [0; 1024];
        assert_eq!(
            MqttSerializer::to_buffer(&mut buffer, &pubrel).unwrap(),
            good_pubcomp
        );
    }
}
