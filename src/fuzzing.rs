use crate::{
    QoS, ReasonCode,
    de::{PacketReader, received_packet::ReceivedPacket},
    packets::{
        Connect, DisconnectReq, PingReq, PubAck, PubComp, PubRec, PubRel, Reason, Subscribe,
        Unsubscribe,
    },
    properties::Property,
    publication::Publication,
    ser::MqttSerializer,
    types::{BinaryData, Properties, TopicFilter, Utf8String},
    varint::Varint,
};

/// Parse one inbound packet buffer.
pub fn parse_received_packet(buf: &[u8]) {
    let _ = ReceivedPacket::from_buffer(buf);
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
            let _ = reader.received_packet();
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
static PUBLISH_RESPONSE_TOPIC: [Property<'static>; 1] = [Property::ResponseTopic(Utf8String("r"))];
static PUBLISH_USER_PROPERTY: [Property<'static>; 1] =
    [Property::UserProperty(Utf8String("k"), Utf8String("v"))];
static PUBLISH_CONTENT_TYPE: [Property<'static>; 1] = [Property::ContentType(Utf8String("t"))];
static SUBSCRIBE_ID: [Property<'static>; 1] = [Property::SubscriptionIdentifier(Varint(1))];
static SHARED_USER_PROPERTY: [Property<'static>; 1] =
    [Property::UserProperty(Utf8String("a"), Utf8String("b"))];
static CONNECT_SESSION: [Property<'static>; 1] = [Property::SessionExpiryInterval(1)];
static CONNECT_LIMITS: [Property<'static>; 3] = [
    Property::ReceiveMaximum(2),
    Property::MaximumPacketSize(32),
    Property::RequestProblemInformation(1),
];
static REASON_STRING: [Property<'static>; 1] = [Property::ReasonString(Utf8String("r"))];
static REASON_USER_PROPERTY: [Property<'static>; 1] =
    [Property::UserProperty(Utf8String("x"), Utf8String("y"))];

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

fn reason(sel: u8) -> Reason<'static> {
    let code = ReasonCode::from(sel);
    match (sel >> 4) & 0b11 {
        0 => code.into(),
        1 => Reason::with_properties(code, &REASON_STRING),
        _ => Reason::with_properties(code, &REASON_USER_PROPERTY),
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
    match tag % 10 {
        0 => {
            let _ = MqttSerializer::to_buffer(buf, &PingReq);
        }
        1 => {
            let mut publication = Publication::new(topic, payload).qos(fuzz_qos(qos));
            if retain {
                publication = publication.retain();
            }
            publication = publication.properties(publish_properties(aux));
            if aux & 0b100 != 0 {
                publication = publication.correlate(BinaryData(b"id").0);
            }

            let mut publish = crate::packets::Pub::from(publication);
            publish.packet_id = (publish.qos > QoS::AtMostOnce).then_some(1);
            let _ = MqttSerializer::pub_to_buffer(buf, publish);
        }
        2 => {
            let topics = [TopicFilter::new(topic)];
            let _ = MqttSerializer::to_buffer(
                buf,
                &Subscribe {
                    packet_id: 1,
                    dup: retain,
                    properties: Properties::Slice(subscribe_properties(aux)),
                    topics: &topics,
                },
            );
        }
        3 => {
            let topics = [topic];
            let _ = MqttSerializer::to_buffer(
                buf,
                &Unsubscribe {
                    packet_id: 1,
                    dup: retain,
                    properties: Properties::Slice(generic_properties(aux)),
                    topics: &topics,
                },
            );
        }
        4 => {
            let _ = MqttSerializer::to_buffer(
                buf,
                &Connect {
                    keepalive: u16::from(aux),
                    properties: Properties::Slice(connect_properties(aux >> 4)),
                    client_id: Utf8String(topic),
                    auth: None,
                    will: None,
                    clean_start: retain,
                },
            );
        }
        5 => {
            let _ = MqttSerializer::to_buffer(
                buf,
                &PubRel {
                    packet_id: 1,
                    reason: reason(aux),
                },
            );
        }
        6 => {
            let _ = MqttSerializer::to_buffer(
                buf,
                &PubAck {
                    packet_identifier: 1,
                    reason: reason(aux),
                },
            );
        }
        7 => {
            let _ = MqttSerializer::to_buffer(
                buf,
                &PubRec {
                    packet_id: 1,
                    reason: reason(aux),
                },
            );
        }
        8 => {
            let _ = MqttSerializer::to_buffer(
                buf,
                &PubComp {
                    packet_id: 1,
                    reason: reason(aux),
                },
            );
        }
        _ => {
            let _ = MqttSerializer::to_buffer(buf, &DisconnectReq);
        }
    }
}
