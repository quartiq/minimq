mod support;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use minimq::{
    Broker, Buffers, ConfigBuilder, Error, Event, ProtocolError, PubError, Publication, QoS,
    Session, transport::Connector,
};
use std::{cell::RefCell, collections::VecDeque};
use support::block_on;

#[derive(Default)]
struct MockConnection {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
    write_error: Option<(usize, ErrorKind)>,
}

impl MockConnection {
    fn push_rx(&mut self, data: &[u8]) {
        self.rx.push_back(data.to_vec());
    }

    fn fail_write_after(&mut self, successful_writes: usize, err: ErrorKind) {
        self.write_error = Some((successful_writes, err));
    }
}

impl ErrorType for MockConnection {
    type Error = ErrorKind;
}

impl Read for MockConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let Some(mut chunk) = self.rx.pop_front() else {
            return Err(ErrorKind::TimedOut);
        };

        let len = buf.len().min(chunk.len());
        buf[..len].copy_from_slice(&chunk[..len]);
        if len < chunk.len() {
            chunk.drain(..len);
            self.rx.push_front(chunk);
        }
        Ok(len)
    }
}

impl Write for MockConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if let Some((remaining, err)) = &mut self.write_error {
            if *remaining == 0 {
                let err = *err;
                self.write_error = None;
                return Err(err);
            }
            *remaining -= 1;
        }
        self.tx.push(buf.to_vec());
        Ok(buf.len())
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}

struct MockConnector {
    connections: RefCell<VecDeque<MockConnection>>,
}

impl MockConnector {
    fn new(connection: MockConnection) -> Self {
        Self {
            connections: RefCell::new(VecDeque::from([connection])),
        }
    }

    fn with_connections(connections: impl IntoIterator<Item = MockConnection>) -> Self {
        Self {
            connections: RefCell::new(connections.into_iter().collect()),
        }
    }
}

impl Connector for MockConnector {
    type Error = ErrorKind;
    type Connection<'a> = MockConnection;

    async fn connect<'a>(&'a self, _broker: &Broker) -> Result<Self::Connection<'a>, Error> {
        self.connections
            .borrow_mut()
            .pop_front()
            .ok_or(Error::Transport(ErrorKind::ConnectionReset))
    }
}

fn config() -> minimq::Config<'static> {
    let rx = Box::leak(Box::new([0; 128]));
    let tx = Box::leak(Box::new([0; 128]));
    let inflight = Box::leak(Box::new([0; 1024]));
    let broker = Broker::from("127.0.0.1".parse::<std::net::IpAddr>().unwrap());
    ConfigBuilder::new(broker, Buffers { rx, tx, inflight })
        .client_id("test")
        .unwrap()
        .build()
}

fn connack() -> [u8; 5] {
    [0x20, 0x03, 0x00, 0x00, 0x00]
}

fn connack_receive_max(max: u16) -> [u8; 8] {
    [
        0x20,
        0x06,
        0x00,
        0x00,
        0x03,
        0x21,
        (max >> 8) as u8,
        max as u8,
    ]
}

fn puback(id: u16) -> [u8; 4] {
    [0x40, 0x02, (id >> 8) as u8, id as u8]
}

fn pubrec(id: u16) -> [u8; 4] {
    [0x50, 0x02, (id >> 8) as u8, id as u8]
}

fn pubcomp(id: u16) -> [u8; 4] {
    [0x70, 0x02, (id >> 8) as u8, id as u8]
}

fn pubrel(id: u16) -> [u8; 4] {
    [0x62, 0x02, (id >> 8) as u8, id as u8]
}

fn disconnect() -> [u8; 4] {
    [0xE0, 0x02, 0x00, 0x00]
}

fn connack_session_present() -> [u8; 5] {
    [0x20, 0x03, 0x01, 0x00, 0x00]
}

fn inbound_publish_qos2(id: u16, topic: &str, payload: &[u8]) -> Vec<u8> {
    let remaining = 2 + topic.len() + 2 + 1 + payload.len();
    let mut packet = Vec::with_capacity(2 + remaining);
    packet.push(0x34);
    packet.push(remaining as u8);
    packet.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    packet.extend_from_slice(topic.as_bytes());
    packet.extend_from_slice(&id.to_be_bytes());
    packet.push(0x00);
    packet.extend_from_slice(payload);
    packet
}

fn inbound_publish_qos1(id: u16, topic: &str, payload: &[u8]) -> Vec<u8> {
    let remaining = 2 + topic.len() + 2 + 1 + payload.len();
    let mut packet = Vec::with_capacity(2 + remaining);
    packet.push(0x32);
    packet.push(remaining as u8);
    packet.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    packet.extend_from_slice(topic.as_bytes());
    packet.extend_from_slice(&id.to_be_bytes());
    packet.push(0x00);
    packet.extend_from_slice(payload);
    packet
}

fn connected_session<'a>(connector: &'a MockConnector) -> Session<'a, 'static, MockConnector> {
    let mut session = Session::new(config(), connector);
    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
    assert!(session.is_connected());
    session
}

#[test]
fn publish_connects_session_on_demand() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = Session::new(config(), &connector);
    block_on(session.publish(Publication::new("data", b"payload").qos(QoS::AtLeastOnce))).unwrap();
    assert!(session.is_connected());
}

#[test]
fn inflight_packets_are_not_replayed_during_normal_poll() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    let tx_after_publish = connector.connections.borrow().front().is_none();
    assert!(tx_after_publish);

    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
}

#[test]
fn broker_disconnect_marks_inflight_for_replay_on_resume() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());
    let second = {
        let mut second = MockConnection::default();
        second.push_rx(&connack_session_present());
        second
    };
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config(), &connector);

    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
    block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Reconnected
    ));
}

#[test]
fn inflight_metadata_exhaustion_is_reported() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    for index in 0..16 {
        block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce)))
            .unwrap_or_else(|_| panic!("publish {index} failed"));
    }

    let result = block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce)));
    assert!(matches!(
        result,
        Err(PubError::Error(Error::Protocol(
            ProtocolError::InflightMetadataExhausted
        )))
    ));
}

#[test]
fn outbound_qos_acks_can_arrive_out_of_order() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&puback(2));
    connection.push_rx(&puback(1));
    connection.push_rx(&puback(3));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    for _ in 0..3 {
        block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    }
    assert!(session.pending_messages());

    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(!session.pending_messages());
}

#[test]
fn outbound_qos2_flow_allows_out_of_order_pubrec_and_pubcomp() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec(2));
    connection.push_rx(&pubrec(1));
    connection.push_rx(&pubcomp(2));
    connection.push_rx(&pubcomp(1));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    for _ in 0..2 {
        block_on(session.publish(Publication::new("data", b"x").qos(QoS::ExactlyOnce))).unwrap();
    }

    for _ in 0..4 {
        assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    }
    assert!(!session.pending_messages());
}

#[test]
fn inbound_qos2_marks_pending_until_pubrel() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    connection.push_rx(&pubrel(9));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let message = match block_on(session.poll()).unwrap() {
        Event::Inbound(message) => message,
        other => panic!("unexpected event: {other:?}"),
    };
    assert_eq!(message.topic, "data");
    assert!(session.pending_messages());

    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(!session.pending_messages());
}

#[test]
fn connack_receive_maximum_clamps_local_quota() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack_receive_max(2));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();

    let result = block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce)));
    assert!(matches!(result, Err(PubError::Error(Error::NotReady))));
}

#[test]
fn session_allows_publish_after_message_borrow_is_dropped() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos1(7, "cmd", b"payload"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    {
        let message = match block_on(session.poll()).unwrap() {
            Event::Inbound(message) => message,
            other => panic!("unexpected event: {other:?}"),
        };
        assert_eq!(message.topic, "cmd");
        assert_eq!(message.payload, b"payload");
    }

    block_on(session.publish(Publication::new("reply", b"ok").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn session_reconnects_after_write_error() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.fail_write_after(1, ErrorKind::ConnectionReset);

    let mut second = MockConnection::default();
    second.push_rx(&connack_session_present());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config(), &connector);

    let error = block_on(session.publish(Publication::new("reply", b"ok").qos(QoS::AtLeastOnce)))
        .unwrap_err();
    assert!(matches!(
        error,
        PubError::Error(Error::Transport(ErrorKind::ConnectionReset))
    ));

    block_on(session.publish(Publication::new("reply", b"ok").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn poll_reports_reconnect() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = Session::new(config(), &connector);

    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
}

#[test]
fn inbound_publish_exposes_response_helpers() {
    let props = [
        minimq::Property::ResponseTopic(minimq::types::Utf8String("reply/topic")),
        minimq::Property::CorrelationData(minimq::types::BinaryData(b"abc")),
    ];
    let message = minimq::InboundPublish {
        topic: "cmd",
        payload: b"payload",
        properties: minimq::types::Properties::Slice(&props),
        retain: minimq::Retain::NotRetained,
        qos: QoS::AtMostOnce,
    };

    assert_eq!(message.response_topic(), Some("reply/topic"));
    assert_eq!(message.correlation_data(), Some(&b"abc"[..]));
    let target = message.response_target().unwrap();
    assert_eq!(target.topic(), "reply/topic");
    assert_eq!(target.correlation_data(), Some(&b"abc"[..]));
    let owned_via_message = message.reply_owned::<64, 8>().unwrap().unwrap();
    assert_eq!(owned_via_message.topic(), "reply/topic");
    assert_eq!(owned_via_message.correlation_data(), Some(&b"abc"[..]));

    let reply = message.reply("ok").unwrap().qos(QoS::AtLeastOnce);
    let owned = target.to_owned::<64, 8>().unwrap();
    let follow_up = owned.publication("next");

    let response = match reply.properties_ref() {
        minimq::types::Properties::CorrelatedSlice { correlation, .. } => correlation,
        _ => panic!("reply should preserve correlation data"),
    };
    assert!(matches!(
        response,
        minimq::Property::CorrelationData(minimq::types::BinaryData(b"abc"))
    ));
    assert_eq!(follow_up.topic(), "reply/topic");
}
