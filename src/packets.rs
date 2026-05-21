use crate::{
    Properties, Property, QoS, Retain,
    reason_codes::ReasonCode,
    types::{Auth, TopicFilter},
    will::Will,
    wire::{BinaryData, Utf8String},
};
use serde::{Deserialize, Serialize};

use serde::ser::SerializeStruct;

#[derive(Debug)]
pub(crate) struct Connect<'a> {
    pub(crate) keepalive: u16,
    pub(crate) properties: Properties<'a>,
    pub(crate) client_id: Utf8String<'a>,
    #[allow(dead_code)]
    pub(crate) auth: Option<Auth<'a>>,
    pub(crate) will: Option<Will<'a>>,
    pub(crate) clean_start: bool,
}

impl serde::Serialize for Connect<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut flags: u8 = 0;
        if self.clean_start {
            flags |= 1 << 1;
        }

        if let Some(will) = &self.will {
            // Update the flags for the will parameters. Indicate that the will is present, the QoS of
            // the will message, and whether or not the will message should be retained.
            flags |= 1 << 2;
            flags |= (will.qos_level() as u8) << 3;
            if will.retained_flag() == Retain::Retained {
                flags |= 1 << 5;
            }
        }

        if self.auth.is_some() {
            flags |= 1 << 6;
            flags |= 1 << 7;
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
            item.serialize_field("user_name", &Utf8String(auth.user_name()))?;
            item.serialize_field("password", &BinaryData(auth.password()))?;
        }

        item.end()
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ConnAck<'a> {
    pub(crate) session_present: bool,
    pub(crate) reason_code: ReasonCode,
    #[serde(borrow)]
    pub(crate) properties: Properties<'a>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PublishHeader<'a> {
    pub(crate) topic: Utf8String<'a>,
    pub(crate) packet_id: Option<u16>,
    pub(crate) properties: Properties<'a>,
    #[serde(skip)]
    pub(crate) retain: Retain,
    #[serde(skip)]
    pub(crate) qos: QoS,
    #[serde(skip)]
    pub(crate) dup: bool,
}

pub(crate) struct Publish<'a, P> {
    pub(crate) topic: Utf8String<'a>,
    pub(crate) packet_id: Option<u16>,
    pub(crate) properties: Properties<'a>,
    pub(crate) payload: P,
    pub(crate) retain: Retain,
    pub(crate) qos: QoS,
    pub(crate) dup: bool,
}

impl<P> core::fmt::Debug for Publish<'_, P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Publish")
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

#[derive(Debug, Serialize)]
pub(crate) struct Subscribe<'a> {
    pub(crate) packet_id: u16,
    #[serde(skip)]
    pub(crate) dup: bool,
    pub(crate) properties: Properties<'a>,
    pub(crate) topics: &'a [TopicFilter<'a>],
}

#[derive(Debug)]
pub(crate) struct Unsubscribe<'a> {
    pub(crate) packet_id: u16,
    pub(crate) dup: bool,
    pub(crate) properties: Properties<'a>,
    pub(crate) topics: &'a [&'a str],
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
pub(crate) struct PingReq;

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct PingResp;

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PubAck<'a> {
    pub(crate) packet_id: u16,
    #[serde(borrow)]
    pub(crate) reason: Reason<'a>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct SubAck<'a> {
    pub(crate) packet_id: u16,
    #[serde(borrow)]
    pub(crate) properties: Properties<'a>,
    #[serde(skip)]
    pub(crate) codes: &'a [u8],
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub(crate) struct UnsubAck<'a> {
    pub(crate) packet_id: u16,
    #[serde(borrow)]
    pub(crate) properties: Properties<'a>,
    #[serde(skip)]
    pub(crate) codes: &'a [u8],
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PubRec<'a> {
    pub(crate) packet_id: u16,
    #[serde(borrow)]
    pub(crate) reason: Reason<'a>,
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct PubRel<'a> {
    pub(crate) packet_id: u16,
    #[serde(borrow)]
    pub(crate) reason: Reason<'a>,
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct PubComp<'a> {
    pub(crate) packet_id: u16,
    #[serde(borrow)]
    pub(crate) reason: Reason<'a>,
}

/// MQTT `DISCONNECT` packet.
#[derive(Debug, Serialize)]
pub struct Disconnect<'a> {
    reason_code: Option<ReasonCode>,
    properties: Option<Properties<'a>>,
}

impl Default for Disconnect<'_> {
    fn default() -> Self {
        Self::success()
    }
}

impl Disconnect<'_> {
    /// Construct a minimal successful `DISCONNECT`.
    pub const fn success() -> Self {
        Self {
            reason_code: None,
            properties: None,
        }
    }

    /// Construct a `DISCONNECT` with an explicit reason code.
    pub const fn with_reason(reason_code: ReasonCode) -> Self {
        Self {
            reason_code: Some(reason_code),
            properties: None,
        }
    }

    /// Ask the broker to publish the configured Will immediately.
    pub const fn with_will() -> Self {
        Self::with_reason(ReasonCode::DisconnectWithWill)
    }
}

impl<'a> Disconnect<'a> {
    /// Attach MQTT v5 DISCONNECT properties.
    pub const fn with_properties(mut self, properties: &'a [Property<'a>]) -> Self {
        if self.reason_code.is_none() {
            self.reason_code = Some(ReasonCode::Success);
        }
        self.properties = Some(Properties::from_slice(properties));
        self
    }

    /// Return the reason code, defaulting to `Success` when omitted on the wire.
    pub fn reason_code(&self) -> ReasonCode {
        self.reason_code.unwrap_or(ReasonCode::Success)
    }

    /// Return the property block when it was present on the wire or builder.
    pub const fn properties(&self) -> Option<&Properties<'a>> {
        self.properties.as_ref()
    }
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
            reason_code: wire.reason_code,
            properties: wire.properties,
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct Reason<'a> {
    #[serde(borrow)]
    reason: Option<ReasonData<'a>>,
}

impl From<ReasonCode> for Reason<'_> {
    fn from(code: ReasonCode) -> Self {
        Self {
            reason: Some(ReasonData {
                code,
                properties: None,
            }),
        }
    }
}

impl<'a> Reason<'a> {
    #[cfg_attr(not(feature = "fuzzing"), allow(dead_code))]
    pub(crate) fn with_properties(
        code: ReasonCode,
        properties: &'a [crate::properties::Property<'a>],
    ) -> Self {
        Self {
            reason: Some(ReasonData {
                code,
                properties: Some(Properties::from_slice(properties)),
            }),
        }
    }

    pub(crate) fn code(&self) -> ReasonCode {
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
    #[allow(dead_code)]
    pub properties: Option<Properties<'a>>,
}

#[cfg(test)]
mod tests {
    use super::Disconnect;
    use crate::properties::{Properties, Property};
    use crate::reason_codes::ReasonCode;
    use crate::ser::MqttSerializer;
    use crate::{QoS, Retain};
    use crate::{types::TopicFilter, wire::Utf8String};

    #[test]
    pub fn serialize_publish() {
        let good_publish: [u8; 10] = [
            0x30, // Publish message
            0x08, // Remaining length (8)
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
            0x00, // Properties length
            0xAB, 0xCD, // Payload
        ];

        let header = crate::packets::PublishHeader {
            qos: QoS::AtMostOnce,
            packet_id: None,
            dup: false,
            properties: Properties::from_slice(&[]),
            retain: Retain::NotRetained,
            topic: Utf8String("ABC"),
        };
        let payload = &[0xAB, 0xCD][..];

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::encode_publish(&mut buffer, &header, payload).unwrap();

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

        let header = crate::packets::PublishHeader {
            qos: QoS::AtLeastOnce,
            topic: Utf8String("ABC"),
            packet_id: Some(0xBEEF),
            dup: false,
            properties: Properties::from_slice(&[]),
            retain: Retain::NotRetained,
        };
        let payload = &[0xAB, 0xCD][..];

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::encode_publish(&mut buffer, &header, payload).unwrap();

        assert_eq!(message, good_publish);
    }

    #[test]
    pub fn serialize_publish_qos1_dup() {
        let good_publish: [u8; 12] = [
            0x3A, // Publish message, QoS 1, DUP
            0x0a, // Remaining length (10)
            0x00, 0x03, 0x41, 0x42, 0x43, // Topic: ABC
            0xBE, 0xEF, // Packet identifier
            0x00, // Properties length
            0xAB, 0xCD, // Payload
        ];

        let header = crate::packets::PublishHeader {
            qos: QoS::AtLeastOnce,
            topic: Utf8String("ABC"),
            packet_id: Some(0xBEEF),
            dup: true,
            properties: Properties::from_slice(&[]),
            retain: Retain::NotRetained,
        };
        let payload = &[0xAB, 0xCD][..];

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::encode_publish(&mut buffer, &header, payload).unwrap();

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
            properties: Properties::from_slice(&[]),
            topics: &[TopicFilter::new("ABC")],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::encode(&mut buffer, &subscribe).unwrap();

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
            properties: Properties::from_slice(&[]),
            topics: &["ABC"],
        };

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::encode(&mut buffer, &unsubscribe).unwrap();

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

        let header = crate::packets::PublishHeader {
            qos: QoS::AtMostOnce,
            topic: Utf8String("ABC"),
            packet_id: None,
            dup: false,
            properties: Properties::from_slice(&[Property::ResponseTopic("A")]),
            retain: Retain::NotRetained,
        };
        let payload = &[0xAB, 0xCD][..];

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::encode_publish(&mut buffer, &header, payload).unwrap();

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

        let header = crate::packets::PublishHeader {
            qos: QoS::AtMostOnce,
            topic: Utf8String("ABC"),
            packet_id: None,
            dup: false,
            properties: Properties::from_slice(&[Property::UserProperty("A", "B")]),
            retain: Retain::NotRetained,
        };
        let payload = &[0xAB, 0xCD][..];

        let mut buffer: [u8; 900] = [0; 900];
        let message = MqttSerializer::encode_publish(&mut buffer, &header, payload).unwrap();

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
            client_id: Utf8String("ABC"),
            auth: None,
            will: None,
            keepalive: 10,
            properties: Properties::from_slice(&[]),
            clean_start: true,
        };

        let message = MqttSerializer::encode(&mut buffer, &connect).unwrap();

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
            properties: Properties::from_slice(&[]),
            client_id: Utf8String("ABC"),
            auth: None,
            will: Some(will),
        };

        let message = MqttSerializer::encode(&mut buffer, &connect).unwrap();

        assert_eq!(message, good_serialized_connect)
    }

    #[test]
    fn serialize_ping_req() {
        let good_ping_req: [u8; 2] = [
            0xc0, // Ping
            0x00, // Remaining length (0)
        ];

        let mut buffer: [u8; 1024] = [0; 1024];
        let message = MqttSerializer::encode(&mut buffer, &crate::packets::PingReq {}).unwrap();
        assert_eq!(message, good_ping_req);
    }

    #[test]
    fn serialize_disconnect_success() {
        let mut buffer = [0u8; 16];
        let message = MqttSerializer::encode(&mut buffer, &Disconnect::success()).unwrap();
        assert_eq!(message, [0xe0, 0x00]);
    }

    #[test]
    fn serialize_disconnect_with_will() {
        let mut buffer = [0u8; 16];
        let message = MqttSerializer::encode(&mut buffer, &Disconnect::with_will()).unwrap();
        assert_eq!(message, [0xe0, 0x01, ReasonCode::DisconnectWithWill as u8]);
    }

    #[test]
    fn serialize_disconnect_with_explicit_empty_properties() {
        let mut buffer = [0u8; 16];
        let disconnect = Disconnect::with_reason(ReasonCode::Success).with_properties(&[]);
        let message = MqttSerializer::encode(&mut buffer, &disconnect).unwrap();
        assert_eq!(message, [0xe0, 0x02, ReasonCode::Success as u8, 0x00]);
    }

    #[test]
    fn serialize_pubrel() {
        let good_pubrel: [u8; 5] = [
            6 << 4 | 0b10, // PubRel
            0x03,          // Remaining length
            0x00,
            0x05, // Identifier
            0x10, // Response Code
        ];

        let pubrel = crate::packets::PubRel {
            packet_id: 5,
            reason: ReasonCode::NoMatchingSubscribers.into(),
        };

        let mut buffer: [u8; 1024] = [0; 1024];
        assert_eq!(
            MqttSerializer::encode(&mut buffer, &pubrel).unwrap(),
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
            packet_id: 0x15,
            reason: crate::packets::Reason {
                reason: Some(crate::packets::ReasonData {
                    code: ReasonCode::Success,
                    properties: None,
                }),
            },
        };

        let mut buffer: [u8; 1024] = [0; 1024];
        assert_eq!(
            MqttSerializer::encode(&mut buffer, &pubrel).unwrap(),
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
                    properties: None,
                }),
            },
        };

        let mut buffer: [u8; 1024] = [0; 1024];
        assert_eq!(
            MqttSerializer::encode(&mut buffer, &pubrel).unwrap(),
            good_pubcomp
        );
    }
}
