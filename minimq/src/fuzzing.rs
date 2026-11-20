use crate::{
    QoS, ReasonCode,
    de::{PacketReader, ReceivedPacket},
    mqtt_client::outbound::{ControlAction, serialize_control_packet},
    packets::{Connect, Disconnect, PublishHeader, Subscribe, Unsubscribe},
    properties::{Properties, Property},
    publication::Publication,
    ser::MqttSerializer,
    types::TopicFilter,
    wire::Utf8String,
};

/// Parse one inbound packet buffer.
pub fn parse_received_packet(buf: &[u8]) {
    let _ = ReceivedPacket::from_buffer(buf, crate::Protocol::V5);
    let _ = ReceivedPacket::from_buffer(
        buf,
        crate::Protocol::V3_1_1 {
            clean_session: false,
        },
    );
}

/// Drive `PacketReader` with fragmented input bytes.
pub fn drive_packet_reader(storage: &mut [u8], input: &[u8], fragments: &[u8]) {
    let mut reader = PacketReader::new(storage);
    let mut offset = 0usize;
    let mut fragment_index = 0usize;

    while let Ok(window) = reader.receive_buffer() {
        if window.is_empty() || offset >= input.len() {
            break;
        }

        let requested = fragments.get(fragment_index).copied().unwrap_or(1) as usize;
        fragment_index = fragment_index.saturating_add(1);
        let count = requested.max(1).min(window.len()).min(input.len() - offset);
        window[..count].copy_from_slice(&input[offset..offset + count]);
        reader.commit(count);
        offset += count;

        if reader.packet_available() {
            let _ = reader.received_packet(crate::Protocol::V5);
        }
    }
}

fn fuzz_qos(qos: u8) -> QoS {
    match qos & 0b11 {
        0 => QoS::AtMostOnce,
        1 => QoS::AtLeastOnce,
        _ => QoS::ExactlyOnce,
    }
}

static EMPTY_PROPERTIES: [Property<'static>; 0] = [];
static PUBLISH_RESPONSE_TOPIC: [Property<'static>; 1] = [Property::ResponseTopic("r")];
static PUBLISH_USER_PROPERTY: [Property<'static>; 1] = [Property::UserProperty("k", "v")];
static PUBLISH_CONTENT_TYPE: [Property<'static>; 1] = [Property::ContentType("t")];
static SUBSCRIBE_ID: [Property<'static>; 1] = [Property::SubscriptionIdentifier(1)];
static SHARED_USER_PROPERTY: [Property<'static>; 1] = [Property::UserProperty("a", "b")];
static CONNECT_SESSION: [Property<'static>; 1] = [Property::SessionExpiryInterval(1)];
static CONNECT_LIMITS: [Property<'static>; 3] = [
    Property::ReceiveMaximum(2),
    Property::MaximumPacketSize(32),
    Property::RequestProblemInformation(1),
];
static REASON_STRING: [Property<'static>; 1] = [Property::ReasonString("r")];
static REASON_USER_PROPERTY: [Property<'static>; 1] = [Property::UserProperty("x", "y")];

fn publish_properties(sel: u8) -> &'static [Property<'static>] {
    match sel & 0b11 {
        0 => &EMPTY_PROPERTIES,
        1 => &PUBLISH_RESPONSE_TOPIC,
        2 => &PUBLISH_USER_PROPERTY,
        _ => &PUBLISH_CONTENT_TYPE,
    }
}

fn subscribe_properties(sel: u8) -> &'static [Property<'static>] {
    match sel & 0b1 {
        0 => &EMPTY_PROPERTIES,
        _ => &SUBSCRIBE_ID,
    }
}

fn generic_properties(sel: u8) -> &'static [Property<'static>] {
    match sel & 0b1 {
        0 => &EMPTY_PROPERTIES,
        _ => &SHARED_USER_PROPERTY,
    }
}

fn connect_properties(sel: u8) -> &'static [Property<'static>] {
    match sel & 0b11 {
        0 => &EMPTY_PROPERTIES,
        1 => &CONNECT_SESSION,
        _ => &CONNECT_LIMITS,
    }
}

/// Encode one bounded packet shape into the provided buffer.
pub fn encode_packet(
    buf: &mut [u8],
    tag: u8,
    topic: &str,
    payload: &[u8],
    qos: u8,
    retain: bool,
    aux: u8,
) {
    let protocol = if tag & 0x80 == 0 {
        crate::Protocol::V5
    } else {
        crate::Protocol::V3_1_1 {
            clean_session: false,
        }
    };
    match (tag & 0x7f) % 10 {
        1 => {
            let mut publication = Publication::new(topic, payload).qos(fuzz_qos(qos));
            if retain {
                publication = publication.retain();
            }
            publication = publication.properties(publish_properties(aux));
            if aux & 0b100 != 0 {
                publication = publication.correlate(b"id");
            }

            let header = PublishHeader {
                topic: Utf8String(publication.topic),
                packet_id: (publication.qos > QoS::AtMostOnce).then_some(1),
                properties: protocol.is_v5().then_some(publication.properties),
                retain: publication.retain,
                qos: publication.qos,
                dup: false,
            };
            let _ = MqttSerializer::encode_publish(buf, &header, publication.payload);
        }
        2 => {
            let topics = [TopicFilter::new(topic)];
            let _ = MqttSerializer::encode(
                buf,
                &Subscribe {
                    packet_id: 1,
                    properties: protocol
                        .is_v5()
                        .then(|| Properties::from_slice(subscribe_properties(aux))),
                    topics: &topics,
                },
            );
        }
        3 => {
            let topics = [topic];
            let _ = MqttSerializer::encode(
                buf,
                &Unsubscribe {
                    packet_id: 1,
                    properties: protocol
                        .is_v5()
                        .then(|| Properties::from_slice(generic_properties(aux))),
                    topics: &topics,
                },
            );
        }
        4 => {
            let _ = MqttSerializer::encode(
                buf,
                &Connect {
                    protocol,
                    keepalive: u16::from(aux),
                    properties: Properties::from_slice(connect_properties(aux >> 4)),
                    client_id: Utf8String(topic),
                    auth: None,
                    will: None,
                    clean_start: retain,
                },
            );
        }
        0 | 5..=8 => {
            let packet_id = u16::from(aux) + 1;
            let action = match (tag & 0x7f) % 10 {
                0 => ControlAction::PingReq,
                5 => ControlAction::PubRel { packet_id },
                6 => ControlAction::PubAck {
                    packet_id,
                    reason: ReasonCode::from(aux),
                },
                7 => ControlAction::PubRec { packet_id },
                _ => ControlAction::PubComp {
                    packet_id,
                    reason: ReasonCode::from(aux),
                },
            };
            let mut storage = [0; 5];
            let _ = serialize_control_packet::<core::convert::Infallible>(
                &mut storage,
                action,
                Some(buf.len() as u32),
                protocol,
            );
        }
        _ => {
            let disconnect = Disconnect::with_reason(ReasonCode::from(aux));
            let disconnect = match aux & 3 {
                0 => Disconnect::success(),
                1 => disconnect,
                2 => disconnect.with_properties(&REASON_STRING),
                _ => disconnect.with_properties(&REASON_USER_PROPERTY),
            };
            let _ = MqttSerializer::encode(buf, &disconnect);
        }
    }
}
