mod support;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use minimq::{
    Broker, Buffers, ConfigBuilder, Error, Event, ProtocolError, PubError, Publication, QoS,
    Session, transport::Connector,
};
use std::{cell::RefCell, collections::VecDeque, rc::Rc};
use support::block_on;

#[derive(Default)]
struct MockIo {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
    write_error: Option<(usize, ErrorKind)>,
}

#[derive(Clone, Default)]
struct MockConnection {
    inner: Rc<RefCell<MockIo>>,
}

impl MockConnection {
    fn push_rx(&mut self, data: &[u8]) {
        self.inner.borrow_mut().rx.push_back(data.to_vec());
    }

    fn fail_write_after(&mut self, successful_writes: usize, err: ErrorKind) {
        self.inner.borrow_mut().write_error = Some((successful_writes, err));
    }

    fn tx(&self) -> Vec<Vec<u8>> {
        self.inner.borrow().tx.clone()
    }
}

impl ErrorType for MockConnection {
    type Error = ErrorKind;
}

impl Read for MockConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let Some(mut chunk) = self.inner.borrow_mut().rx.pop_front() else {
            return Err(ErrorKind::TimedOut);
        };

        let len = buf.len().min(chunk.len());
        buf[..len].copy_from_slice(&chunk[..len]);
        if len < chunk.len() {
            chunk.drain(..len);
            self.inner.borrow_mut().rx.push_front(chunk);
        }
        Ok(len)
    }
}

impl Write for MockConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut inner = self.inner.borrow_mut();
        if let Some((remaining, err)) = &mut inner.write_error {
            if *remaining == 0 {
                let err = *err;
                inner.write_error = None;
                return Err(err);
            }
            *remaining -= 1;
        }
        inner.tx.push(buf.to_vec());
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

    async fn connect<'a>(&'a self, _broker: &Broker<'_>) -> Result<Self::Connection<'a>, Error> {
        self.connections
            .borrow_mut()
            .pop_front()
            .ok_or(Error::Transport(ErrorKind::ConnectionReset))
    }
}

fn config() -> minimq::Config<'static> {
    let rx = Box::leak(Box::new([0; 128]));
    let outbound = Box::leak(Box::new([0; 1152]));
    let broker = "127.0.0.1:1883"
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .into();
    ConfigBuilder::new(broker, Buffers { rx, outbound })
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

fn connack_max_packet_size(max: u32) -> [u8; 10] {
    [
        0x20,
        0x08,
        0x00,
        0x00,
        0x05,
        0x27,
        (max >> 24) as u8,
        (max >> 16) as u8,
        (max >> 8) as u8,
        max as u8,
    ]
}

fn puback(id: u16) -> [u8; 4] {
    [0x40, 0x02, (id >> 8) as u8, id as u8]
}

fn puback_reason(id: u16, reason: u8) -> [u8; 5] {
    [0x40, 0x03, (id >> 8) as u8, id as u8, reason]
}

fn pubrec(id: u16) -> [u8; 4] {
    [0x50, 0x02, (id >> 8) as u8, id as u8]
}

fn pubrec_reason(id: u16, reason: u8) -> [u8; 5] {
    [0x50, 0x03, (id >> 8) as u8, id as u8, reason]
}

fn pubcomp(id: u16) -> [u8; 4] {
    [0x70, 0x02, (id >> 8) as u8, id as u8]
}

fn pubcomp_reason(id: u16, reason: u8) -> [u8; 5] {
    [0x70, 0x03, (id >> 8) as u8, id as u8, reason]
}

fn pubrel(id: u16) -> [u8; 4] {
    [0x62, 0x02, (id >> 8) as u8, id as u8]
}

fn disconnect() -> [u8; 4] {
    [0xE0, 0x02, 0x00, 0x00]
}

fn suback(id: u16, code: u8) -> [u8; 6] {
    [0x90, 0x04, (id >> 8) as u8, id as u8, 0x00, code]
}

fn unsuback(id: u16, code: u8) -> [u8; 6] {
    [0xB0, 0x04, (id >> 8) as u8, id as u8, 0x00, code]
}

fn pingreq() -> [u8; 2] {
    [0xC0, 0x00]
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

fn outbound_puback(id: u16) -> [u8; 6] {
    [0x40, 0x04, (id >> 8) as u8, id as u8, 0x00, 0x00]
}

fn outbound_pubrec(id: u16) -> [u8; 6] {
    [0x50, 0x04, (id >> 8) as u8, id as u8, 0x00, 0x00]
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
    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
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
fn broker_disconnect_without_session_resume_reports_connected() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config(), &connector);

    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
}

#[test]
fn inflight_metadata_exhaustion_is_reported() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    for index in 0..4 {
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
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
}

#[test]
fn subscribe_is_replayed_after_disconnect_until_suback() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    let inspect = second.clone();
    second.push_rx(&connack_session_present());
    second.push_rx(&suback(1, 0x01));

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config(), &connector);

    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
    block_on(session.subscribe(&[minimq::types::TopicFilter::new("data")], &[])).unwrap();
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Reconnected
    ));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));

    assert_eq!(
        inspect.tx().last().unwrap(),
        &vec![
            0x8A, 0x0A, 0x00, 0x01, 0x00, 0x00, 0x04, b'd', b'a', b't', b'a', 0x00
        ]
    );
}

#[test]
fn subscribe_reports_inflight_metadata_exhaustion_before_send() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    for _ in 0..4 {
        block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    }

    let result = block_on(session.subscribe(&[minimq::types::TopicFilter::new("data")], &[]));
    assert!(matches!(
        result,
        Err(Error::Protocol(ProtocolError::InflightMetadataExhausted))
    ));
    assert_eq!(inspect.tx().len(), 5);
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

    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
}

#[test]
fn full_retained_outbound_still_sends_puback() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos1(9, "data", b"x"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    for _ in 0..4 {
        block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    }

    match block_on(session.poll()).unwrap() {
        Event::Inbound(message) => assert_eq!(message.topic, "data"),
        other => panic!("unexpected event: {other:?}"),
    }

    assert_eq!(inspect.tx().last().unwrap(), &outbound_puback(9));
}

#[test]
fn full_retained_outbound_still_sends_pubrec() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    for _ in 0..4 {
        block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    }

    match block_on(session.poll()).unwrap() {
        Event::Inbound(message) => assert_eq!(message.topic, "data"),
        other => panic!("unexpected event: {other:?}"),
    }

    assert_eq!(inspect.tx().last().unwrap(), &outbound_pubrec(9));
}

#[test]
fn full_retained_outbound_still_sends_pingreq() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = Session::new(
        ConfigBuilder::new(
            "127.0.0.1:1883"
                .parse::<std::net::SocketAddr>()
                .unwrap()
                .into(),
            Buffers {
                rx: Box::leak(Box::new([0; 128])),
                outbound: Box::leak(Box::new([0; 1152])),
            },
        )
        .client_id("test")
        .unwrap()
        .keepalive_interval(1)
        .build(),
        &connector,
    );

    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
    for _ in 0..4 {
        block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    }

    std::thread::sleep(std::time::Duration::from_millis(600));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(
        inspect
            .tx()
            .iter()
            .any(|frame| frame.as_slice() == pingreq())
    );
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
fn puback_failure_is_reported_and_clears_inflight() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&puback_reason(1, 0x87));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::new("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    assert!(matches!(
        block_on(session.poll()),
        Err(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::NotAuthorized
        )))
    ));
    block_on(session.publish(Publication::new("data", b"y").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn pubrec_failure_is_reported_and_clears_inflight() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec_reason(1, 0x97));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::new("data", b"x").qos(QoS::ExactlyOnce))).unwrap();
    assert!(matches!(
        block_on(session.poll()),
        Err(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::QuotaExceeded
        )))
    ));
    block_on(session.publish(Publication::new("data", b"y").qos(QoS::ExactlyOnce))).unwrap();
}

#[test]
fn pubcomp_failure_is_reported_and_clears_release_state() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec(1));
    connection.push_rx(&pubcomp_reason(1, 0x83));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::new("data", b"x").qos(QoS::ExactlyOnce))).unwrap();
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(
        block_on(session.poll()),
        Err(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::ImplementationError
        )))
    ));
    block_on(session.publish(Publication::new("data", b"y").qos(QoS::ExactlyOnce))).unwrap();
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
fn connect_uses_configured_session_expiry_interval() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let broker = "127.0.0.1:1883"
        .parse::<std::net::SocketAddr>()
        .unwrap()
        .into();
    let mut session = Session::new(
        ConfigBuilder::new(
            broker,
            Buffers {
                rx: Box::leak(Box::new([0; 128])),
                outbound: Box::leak(Box::new([0; 1152])),
            },
        )
        .client_id("test")
        .unwrap()
        .session_expiry_interval(7)
        .build(),
        &connector,
    );

    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
    assert!(
        inspect
            .tx()
            .first()
            .unwrap()
            .windows(5)
            .any(|window| window == [0x11, 0x00, 0x00, 0x00, 0x07])
    );
}

#[test]
fn publish_rejects_packets_larger_than_broker_maximum() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_packet_size(8));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let result = block_on(session.publish(Publication::new("data", b"x")));
    assert!(matches!(
        result,
        Err(PubError::Error(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::PacketTooLarge
        ))))
    ));
    assert_eq!(inspect.tx().len(), 1);
}

#[test]
fn unsubscribe_replays_until_unsuback() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    let inspect = second.clone();
    second.push_rx(&connack_session_present());
    second.push_rx(&unsuback(1, 0x00));

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config(), &connector);

    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Connected
    ));
    block_on(session.unsubscribe(&["data"], &[])).unwrap();
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));
    assert!(matches!(
        block_on(session.poll()).unwrap(),
        Event::Reconnected
    ));
    assert!(matches!(block_on(session.poll()).unwrap(), Event::Idle));

    assert_eq!(
        inspect.tx().last().unwrap(),
        &vec![
            0xAA, 0x09, 0x00, 0x01, 0x00, 0x00, 0x04, b'd', b'a', b't', b'a'
        ]
    );
}

#[test]
fn unsubscribe_rejects_packets_larger_than_broker_maximum() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_packet_size(8));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let result = block_on(session.unsubscribe(&["data"], &[]));
    assert!(matches!(
        result,
        Err(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::PacketTooLarge
        )))
    ));
    assert_eq!(inspect.tx().len(), 1);
}

#[test]
fn subscribe_rejects_packets_larger_than_broker_maximum() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_packet_size(8));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let result = block_on(session.subscribe(&[minimq::types::TopicFilter::new("data")], &[]));
    assert!(matches!(
        result,
        Err(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::PacketTooLarge
        )))
    ));
    assert_eq!(inspect.tx().len(), 1);
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
    let owned_via_message = message.reply_owned::<64, 8>().unwrap().unwrap();
    assert_eq!(owned_via_message.topic(), "reply/topic");
    assert_eq!(owned_via_message.correlation_data(), Some(&b"abc"[..]));

    let reply = message.reply("ok").unwrap().qos(QoS::AtLeastOnce);
    let follow_up = owned_via_message.publication("next");

    let response = match reply.properties_ref() {
        minimq::types::Properties::CorrelatedSlice { correlation, .. } => correlation,
        _ => panic!("reply should preserve correlation data"),
    };
    assert!(matches!(
        response,
        minimq::Property::CorrelationData(minimq::types::BinaryData(b"abc"))
    ));
    assert_eq!(owned_via_message.topic(), "reply/topic");
    let _ = follow_up;
}
