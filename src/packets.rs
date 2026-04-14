use crate::{
    QoS, Retain,
    publication::Publication,
    reason_codes::ReasonCode,
    types::{Auth, BinaryData, Properties, TopicFilter, Utf8String},
    will::Will,
};
use bit_field::BitField;
use serde::{Deserialize, Serialize};

use serde::ser::SerializeStruct;

#[derive(Debug)]
pub struct Connect<'a> {
    pub keepalive: u16,
    pub properties: Properties<'a>,
    pub client_id: Utf8String<'a>,
    #[allow(dead_code)]
    pub auth: Option<Auth<'a>>,
    pub(crate) will: Option<Will<'a>>,
    pub clean_start: bool,
}

impl serde::Serialize for Connect<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut flags: u8 = 0;
        flags.set_bit(1, self.clean_start);

        if let Some(will) = &self.will {
            // Update the flags for the will parameters. Indicate that the will is present, the QoS of
            // the will message, and whether or not the will message should be retained.
            flags.set_bit(2, true);
            flags.set_bits(3..=4, will.qos_level() as u8);
            flags.set_bit(5, will.retained_flag() == Retain::Retained);
        }

        if self.auth.is_some() {
            flags.set_bit(6, true);
            flags.set_bit(7, true);
        }

        let mut item = serializer.serialize_struct("Connect", 0)?;
        item.serialize_field("protocol_name", &Utf8String("MQTT"))?;
        item.serialize_field("protocol_version", &5u8)?;
        item.serialize_field("flags", &flags)?;
        item.serialize_field("keep_alive", &self.keepalive)?;
        item.serialize_field("properties", &self.properties)?;
        item.serialize_field("client_id", &self.client_id)?;
        if let Some(will) = &self.will {
            item.serialize_field("will", will)?;
        }

        if let Some(auth) = &self.auth {
            item.serialize_field("user_name", &Utf8String(auth.user_name))?;
            item.serialize_field("password", &BinaryData(auth.password))?;
        }

        item.end()
    }
}

#[derive(Debug, Deserialize)]
pub struct ConnAck<'a> {
    pub session_present: bool,
    pub reason_code: ReasonCode,
    #[serde(borrow)]
    pub properties: Properties<'a>,
}

#[derive(Serialize)]
pub struct Pub<'a, P> {
    pub topic: Utf8String<'a>,
    pub packet_id: Option<u16>,
    pub properties: Properties<'a>,
    #[serde(skip)]
    pub payload: P,
    #[serde(skip)]
    pub retain: Retain,
    #[serde(skip)]
    pub qos: QoS,
    #[serde(skip)]
    pub dup: bool,
}

impl<P> core::fmt::Debug for Pub<'_, P> {
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

impl<'a, P> From<Publication<'a, P>> for Pub<'a, P> {
    fn from(publication: Publication<'a, P>) -> Self {
        Self {
            topic: Utf8String(publication.topic),
            properties: publication.properties,
            packet_id: None,
            payload: publication.payload,
            retain: publication.retain,
            qos: publication.qos,
            dup: false,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Subscribe<'a> {
    pub packet_id: u16,
    #[serde(skip)]
    pub dup: bool,
    pub properties: Properties<'a>,
    pub topics: &'a [TopicFilter<'a>],
}

#[derive(Debug)]
pub struct Unsubscribe<'a> {
    pub packet_id: u16,
    pub dup: bool,
    pub properties: Properties<'a>,
    pub topics: &'a [&'a str],
}

impl serde::Serialize for Unsubscribe<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut item = serializer.serialize_struct("Unsubscribe", 0)?;
        item.serialize_field("packet_id", &self.packet_id)?;
        item.serialize_field("properties", &self.properties)?;
        item.serialize_field("topics", &TopicFilters(self.topics))?;
        item.end()
    }
}

struct TopicFilters<'a>(&'a [&'a str]);

impl serde::Serialize for TopicFilters<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;

        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for topic in self.0 {
            seq.serialize_element(&Utf8String(topic))?;
        }
        seq.end()
    }
}

#[derive(Debug, Serialize)]
pub struct PingReq;

#[derive(Debug, Serialize)]
pub struct DisconnectReq;

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct PingResp;

#[derive(Debug, Deserialize, Serialize)]
pub struct PubAck<'a> {
    pub packet_identifier: u16,
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SubAck<'a> {
    pub packet_identifier: u16,
    #[serde(borrow)]
    pub properties: Properties<'a>,
    #[serde(skip)]
    pub codes: &'a [u8],
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct UnsubAck<'a> {
    pub packet_identifier: u16,
    #[serde(borrow)]
    pub properties: Properties<'a>,
    #[serde(skip)]
    pub codes: &'a [u8],
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PubRec<'a> {
    pub packet_id: u16,
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PubRel<'a> {
    pub packet_id: u16,
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PubComp<'a> {
    pub packet_id: u16,
    #[serde(borrow)]
    pub reason: Reason<'a>,
}

#[allow(dead_code)]
#[derive(Debug)]
pub struct Disconnect<'a> {
    pub reason_code: ReasonCode,
    pub properties: Properties<'a>,
}

impl<'a, 'de: 'a> Deserialize<'de> for Disconnect<'a> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire<'a> {
            #[serde(default)]
            reason_code: Option<ReasonCode>,
            #[serde(borrow, default)]
            properties: Option<Properties<'a>>,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            reason_code: wire.reason_code.unwrap_or(ReasonCode::Success),
            properties: wire.properties.unwrap_or(Properties::Slice(&[])),
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Reason<'a> {
    #[serde(borrow)]
    reason: Option<ReasonData<'a>>,
}

impl From<ReasonCode> for Reason<'_> {
    fn from(code: ReasonCode) -> Self {
        Self {
            reason: Some(ReasonData {
                code,
                _properties: Some(Properties::Slice(&[])),
            }),
        }
    }
}

impl Reason<'_> {
    pub fn code(&self) -> ReasonCode {
        self.reason
            .as_ref()
            .map(|data| data.code)
            .unwrap_or(ReasonCode::Success)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ReasonData<'a> {
    pub code: ReasonCode,
    #[serde(borrow)]
    pub _properties: Option<Properties<'a>>,
}

#[cfg(test)]
mod tests {
    use crate::reason_codes::ReasonCode;
    use crate::ser::MqttSerializer;
    use crate::types::TopicFilter;

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
        let message = MqttSerializer::pub_to_buffer(&mut buffer, publish).unwrap();

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
        let message = MqttSerializer::pub_to_buffer(&mut buffer, publish).unwrap();

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
            dup: false,
            properties: crate::types::Properties::Slice(&[]),
            topics: &[TopicFilter::new("ABC")],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::to_buffer(&mut buffer, &subscribe).unwrap();

        assert_eq!(message, good_subscribe);
    }

    #[test]
    fn serialize_unsubscribe() {
        let good_unsubscribe: [u8; 10] = [
            0xA2, // Unsubscribe request
            0x08, // Remaining length (8)
            0x00, 0x10, // Packet identifier (16)
            0x00, // Property length
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
        ];

        let unsubscribe = crate::packets::Unsubscribe {
            packet_id: 16,
            dup: false,
            properties: crate::types::Properties::Slice(&[]),
            topics: &["ABC"],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::to_buffer(&mut buffer, &unsubscribe).unwrap();

        assert_eq!(message, good_unsubscribe);
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
        let message = MqttSerializer::pub_to_buffer(&mut buffer, publish).unwrap();

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
        let message = MqttSerializer::pub_to_buffer(&mut buffer, publish).unwrap();

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
            keepalive: 10,
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
        let will = crate::will::Will::new("EFG", &[0xAB, 0xCD], &[])
            .unwrap()
            .qos(crate::QoS::AtMostOnce);

        let connect = crate::packets::Connect {
            clean_start: true,
            keepalive: 10,
            properties: crate::types::Properties::Slice(&[]),
            client_id: crate::types::Utf8String("ABC"),
            auth: None,
            will: Some(will),
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
