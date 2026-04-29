mod support;

use embassy_time::{Duration, with_timeout};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use minimq::{
    Buffers, ConfigBuilder, ConnectEvent, Error, InboundPublish, ProtocolError, PubError,
    Publication, QoS, Session,
};
use std::{cell::RefCell, collections::VecDeque, future::poll_fn, rc::Rc, task::Poll};
use support::{block_on, poll_once};

const WAIT_STEPS: usize = 64;

#[derive(Default)]
struct MockIo {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
    read_error: Option<ErrorKind>,
    write_error: Option<(usize, ErrorKind)>,
    pending_reads: usize,
    pending_writes: usize,
    pending_flushes: usize,
}

#[derive(Clone, Default)]
struct MockConnection {
    inner: Rc<RefCell<MockIo>>,
}

impl MockConnection {
    fn push_rx(&mut self, data: &[u8]) {
        self.inner.borrow_mut().rx.push_back(data.to_vec());
    }

    fn push_rx_fragmented(&mut self, data: &[u8]) {
        for byte in data {
            self.push_rx(core::slice::from_ref(byte));
        }
    }

    fn fail_write_after(&mut self, successful_writes: usize, err: ErrorKind) {
        self.inner.borrow_mut().write_error = Some((successful_writes, err));
    }

    fn fail_read(&mut self, err: ErrorKind) {
        self.inner.borrow_mut().read_error = Some(err);
    }

    fn pend_reads(&mut self, count: usize) {
        self.inner.borrow_mut().pending_reads = count;
    }

    fn pend_writes(&mut self, count: usize) {
        self.inner.borrow_mut().pending_writes = count;
    }

    fn pend_flushes(&mut self, count: usize) {
        self.inner.borrow_mut().pending_flushes = count;
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
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if inner.pending_reads != 0 {
                inner.pending_reads -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }

            if let Some(err) = inner.read_error.take() {
                return Poll::Ready(Err(err));
            }

            let Some(mut chunk) = inner.rx.pop_front() else {
                return Poll::Pending;
            };

            let len = buf.len().min(chunk.len());
            buf[..len].copy_from_slice(&chunk[..len]);
            if len < chunk.len() {
                chunk.drain(..len);
                inner.rx.push_front(chunk);
            }
            Poll::Ready(Ok(len))
        })
        .await
    }
}

impl Write for MockConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if inner.pending_writes != 0 {
                inner.pending_writes -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            if let Some((remaining, err)) = &mut inner.write_error {
                if *remaining == 0 {
                    let err = *err;
                    inner.write_error = None;
                    return Poll::Ready(Err(err));
                }
                *remaining -= 1;
            }
            inner.tx.push(buf.to_vec());
            Poll::Ready(Ok(buf.len()))
        })
        .await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if inner.pending_flushes != 0 {
                inner.pending_flushes -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            Poll::Ready(Ok(()))
        })
        .await
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

impl MockConnector {
    fn connect(&self) -> Result<MockConnection, ErrorKind> {
        self.connections
            .borrow_mut()
            .pop_front()
            .ok_or(ErrorKind::ConnectionReset)
    }
}

fn config() -> ConfigBuilder<'static> {
    let rx = Box::leak(Box::new([0; 128]));
    let tx = Box::leak(Box::new([0; 1152]));
    ConfigBuilder::new(Buffers::new(rx, tx))
        .client_id("test")
        .unwrap()
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

fn disconnect_req() -> [u8; 2] {
    [0xE0, 0x00]
}

fn suback(id: u16, code: u8) -> [u8; 6] {
    [0x90, 0x04, (id >> 8) as u8, id as u8, 0x00, code]
}

fn unsuback(id: u16, code: u8) -> [u8; 6] {
    [0xB0, 0x04, (id >> 8) as u8, id as u8, 0x00, code]
}

fn connack_session_present() -> [u8; 5] {
    [0x20, 0x03, 0x01, 0x00, 0x00]
}

fn connack_assigned_client_id(client_id: &str) -> Vec<u8> {
    let property_len = 1 + 2 + client_id.len();
    let remaining_len = 2 + 1 + property_len;
    let mut packet = Vec::with_capacity(2 + remaining_len);
    packet.push(0x20);
    packet.push(remaining_len as u8);
    packet.push(0x00);
    packet.push(0x00);
    packet.push(property_len as u8);
    packet.push(0x12);
    packet.extend_from_slice(&(client_id.len() as u16).to_be_bytes());
    packet.extend_from_slice(client_id.as_bytes());
    packet
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

fn inbound_publish_with_response(
    topic: &str,
    payload: &[u8],
    response_topic: &str,
    correlation: &[u8],
) -> Vec<u8> {
    let response_len = 1 + 2 + response_topic.len();
    let correlation_len = 1 + 2 + correlation.len();
    let properties_len = response_len + correlation_len;
    let remaining = 2 + topic.len() + 1 + properties_len + payload.len();
    let mut packet = Vec::with_capacity(2 + remaining);
    packet.push(0x30);
    packet.push(remaining as u8);
    packet.extend_from_slice(&(topic.len() as u16).to_be_bytes());
    packet.extend_from_slice(topic.as_bytes());
    packet.push(properties_len as u8);
    packet.push(0x08);
    packet.extend_from_slice(&(response_topic.len() as u16).to_be_bytes());
    packet.extend_from_slice(response_topic.as_bytes());
    packet.push(0x09);
    packet.extend_from_slice(&(correlation.len() as u16).to_be_bytes());
    packet.extend_from_slice(correlation);
    packet.extend_from_slice(payload);
    packet
}

fn outbound_puback(id: u16) -> [u8; 5] {
    [0x40, 0x03, (id >> 8) as u8, id as u8, 0x00]
}

fn outbound_pubrec(id: u16) -> [u8; 5] {
    [0x50, 0x03, (id >> 8) as u8, id as u8, 0x00]
}

fn outbound_pubrel(id: u16) -> [u8; 5] {
    [0x62, 0x03, (id >> 8) as u8, id as u8, 0x00]
}

fn outbound_pubcomp(id: u16, reason: u8) -> [u8; 5] {
    [0x70, 0x03, (id >> 8) as u8, id as u8, reason]
}

fn connected_session(connector: &MockConnector) -> Session<'static, MockConnection> {
    let mut session = Session::new(config());
    expect_connected(&mut session, connector);
    assert!(session.is_connected());
    session
}

fn expect_connected(session: &mut Session<'static, MockConnection>, connector: &MockConnector) {
    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())).unwrap(),
        ConnectEvent::Connected
    ));
}

fn expect_reconnected(session: &mut Session<'static, MockConnection>, connector: &MockConnector) {
    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())).unwrap(),
        ConnectEvent::Reconnected
    ));
}

fn poll_now<'a>(
    session: &'a mut Session<'static, MockConnection>,
) -> Result<Option<InboundPublish<'a>>, Error<ErrorKind>> {
    match block_on(with_timeout(Duration::from_millis(0), session.poll())) {
        Ok(Ok(event)) => Ok(Some(event)),
        Ok(Err(err)) => Err(err),
        Err(_) => Ok(None),
    }
}

fn expect_disconnected(session: &mut Session<'static, MockConnection>) {
    for _ in 0..WAIT_STEPS {
        match poll_now(session) {
            Err(Error::Disconnected) => return,
            Ok(None) => {}
            other => panic!("unexpected poll result while waiting for disconnect: {other:?}"),
        }
    }
    panic!("timed out waiting for disconnect");
}

fn with_inbound<T>(
    session: &mut Session<'static, MockConnection>,
    f: impl FnOnce(minimq::InboundPublish<'_>) -> T,
) -> T {
    f(block_on(session.poll()).unwrap())
}

fn expect_poll_error(
    session: &mut Session<'static, MockConnection>,
    want: impl Fn(&Error<ErrorKind>) -> bool,
) {
    for _ in 0..WAIT_STEPS {
        match poll_now(session) {
            Ok(None) => {}
            Err(err) if want(&err) => return,
            other => panic!("unexpected poll result while waiting for error: {other:?}"),
        }
    }
    panic!("timed out waiting for poll error");
}

fn wait_for_tx_frame(
    session: &mut Session<'static, MockConnection>,
    inspect: &MockConnection,
    expected: &[u8],
) {
    if inspect
        .tx()
        .iter()
        .any(|frame| frame.as_slice() == expected)
    {
        return;
    }

    for _ in 0..WAIT_STEPS {
        match poll_now(session) {
            Ok(None) | Err(Error::Disconnected) => {}
            other => panic!("unexpected poll result while waiting for tx frame: {other:?}"),
        }
        if inspect
            .tx()
            .iter()
            .any(|frame| frame.as_slice() == expected)
        {
            return;
        }
    }

    panic!("timed out waiting for tx frame");
}

fn fill_inflight_publish_slots(session: &mut Session<'static, MockConnection>) -> usize {
    let mut count = 0;
    loop {
        match block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))) {
            Ok(()) => count += 1,
            Err(PubError::Session(Error::Protocol(ProtocolError::InflightMetadataExhausted))) => {
                return count;
            }
            other => panic!("unexpected publish result while filling inflight slots: {other:?}"),
        }
    }
}

#[test]
fn fragmented_connack_still_establishes_session() {
    let mut connection = MockConnection::default();
    connection.push_rx_fragmented(&connack());
    let connector = MockConnector::new(connection);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    assert!(session.is_connected());
}

#[test]
fn fragmented_inbound_publish_is_assembled_and_acked() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx_fragmented(&inbound_publish_qos1(7, "data", b"payload"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    with_inbound(&mut session, |message| {
        assert_eq!(message.topic(), "data");
        assert_eq!(message.payload(), b"payload");
    });

    wait_for_tx_frame(&mut session, &inspect, &outbound_puback(7));
}

#[test]
fn publish_requires_connected_session() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = Session::new(config());

    let result =
        block_on(session.publish(Publication::bytes("data", b"payload").qos(QoS::AtLeastOnce)));
    assert!(matches!(
        result,
        Err(PubError::Session(Error::Disconnected))
    ));

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"payload").qos(QoS::AtLeastOnce)))
        .unwrap();
}

#[test]
fn publish_survives_cancellation_during_pending_write() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&puback(1));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    inspect.pend_writes(1);
    let mut future =
        Box::pin(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert_eq!(inspect.tx().len(), 1);
    assert!(poll_now(&mut session).unwrap().is_none());
    block_on(session.publish(Publication::bytes("data", b"y").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn cancelled_publish_still_holds_receive_max_quota() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack_receive_max(1));
    connection.push_rx(&puback(1));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    inspect.pend_writes(1);
    let mut future =
        Box::pin(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    let result = block_on(session.publish(Publication::bytes("data", b"y").qos(QoS::AtLeastOnce)));
    assert!(matches!(result, Err(PubError::Session(Error::NotReady))));
    for _ in 0..WAIT_STEPS {
        match block_on(session.publish(Publication::bytes("data", b"z").qos(QoS::AtLeastOnce))) {
            Ok(()) => return,
            Err(PubError::Session(Error::NotReady)) => {
                assert!(poll_now(&mut session).unwrap().is_none());
            }
            other => panic!("unexpected publish result while waiting for quota: {other:?}"),
        }
    }
    panic!("timed out waiting for send quota to recover");
}

#[test]
fn subscribe_survives_cancellation_during_pending_flush() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&suback(1, 0x01));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    inspect.pend_flushes(1);
    let topics = [minimq::types::TopicFilter::new("data")];
    let mut future = Box::pin(session.subscribe(&topics, &[]));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert_eq!(inspect.tx().len(), 2);
    assert!(poll_now(&mut session).unwrap().is_none());
    assert_eq!(inspect.tx().len(), 2);
}

#[test]
fn poll_survives_cancellation_during_pending_read() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    inspect.pend_reads(1);
    let mut future = Box::pin(session.poll());
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert!(session.is_connected());
    assert!(poll_now(&mut session).unwrap().is_none());
}

#[test]
fn qos2_pubrel_survives_cancellation_during_pending_write() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec(1));
    connection.push_rx(&pubcomp(1));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::ExactlyOnce))).unwrap();
    assert!(poll_now(&mut session).unwrap().is_none());

    inspect.pend_writes(1);
    let mut sent_pubrel = false;
    for _ in 0..WAIT_STEPS {
        match poll_now(&mut session).unwrap() {
            None => {
                if inspect
                    .tx()
                    .iter()
                    .any(|frame| frame.as_slice() == outbound_pubrel(1))
                {
                    sent_pubrel = true;
                    break;
                }
            }
            Some(_) => panic!("unexpected inbound event while waiting for PUBREL"),
        }
    }
    assert!(sent_pubrel);

    assert_eq!(inspect.tx().len(), 3);
    assert_eq!(inspect.tx().last().unwrap(), &outbound_pubrel(1));
}

#[test]
fn inflight_packets_are_not_replayed_during_normal_poll() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    let tx_after_publish = connector.connections.borrow().front().is_none();
    assert!(tx_after_publish);

    assert!(poll_now(&mut session).unwrap().is_none());
    assert!(poll_now(&mut session).unwrap().is_none());
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
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
}

#[test]
fn broker_disconnect_without_session_resume_reports_connected() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    expect_disconnected(&mut session);
    expect_connected(&mut session, &connector);
}

#[test]
fn failed_connack_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&[0x20, 0x03, 0x00, 0x87, 0x00]);
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::NotAuthorized
        )))
    ));
    assert!(!session.is_connected());
    expect_connected(&mut session, &connector);
}

#[test]
fn broker_disconnect_during_connect_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&disconnect());
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Disconnected)
    ));
    assert!(!session.is_connected());
    expect_connected(&mut session, &connector);
}

#[test]
fn zero_receive_maximum_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&connack_receive_max(0));
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Protocol(ProtocolError::InvalidProperty))
    ));
    assert!(!session.is_connected());
    expect_connected(&mut session, &connector);
}

#[test]
fn oversized_assigned_client_id_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&connack_assigned_client_id(&"a".repeat(65)));
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Protocol(ProtocolError::ProvidedClientIdTooLong))
    ));
    assert!(!session.is_connected());
    expect_connected(&mut session, &connector);
}

#[test]
fn timed_out_connack_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.fail_read(ErrorKind::TimedOut);
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Transport(ErrorKind::TimedOut))
    ));
    assert!(!session.is_connected());
    expect_connected(&mut session, &connector);
}

#[test]
fn malformed_inbound_packet_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&[0x20, 0xFF, 0xFF, 0xFF, 0xFF]);

    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    expect_poll_error(&mut session, |err| {
        matches!(err, Error::Protocol(ProtocolError::MalformedPacket))
    });
    assert!(!session.is_connected());
    expect_connected(&mut session, &connector);
}

#[test]
fn inflight_metadata_exhaustion_is_reported() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let capacity = fill_inflight_publish_slots(&mut session);
    assert!(capacity > 0);
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
        block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    }
    assert!(poll_now(&mut session).unwrap().is_none());
    assert!(poll_now(&mut session).unwrap().is_none());
    assert!(poll_now(&mut session).unwrap().is_none());
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
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.subscribe(&[minimq::types::TopicFilter::new("data")], &[])).unwrap();
    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    wait_for_tx_frame(
        &mut session,
        &inspect,
        &[
            0x8A, 0x0A, 0x00, 0x01, 0x00, 0x00, 0x04, b'd', b'a', b't', b'a', 0x00,
        ],
    );

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

    let capacity = fill_inflight_publish_slots(&mut session);

    let result = block_on(session.subscribe(&[minimq::types::TopicFilter::new("data")], &[]));
    assert!(matches!(
        result,
        Err(Error::Protocol(ProtocolError::InflightMetadataExhausted))
    ));
    assert_eq!(inspect.tx().len(), capacity + 1);
}

#[test]
fn subscribe_rejects_empty_topic_list() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let result = block_on(session.subscribe(&[], &[]));
    assert!(matches!(
        result,
        Err(Error::Protocol(ProtocolError::NoTopic))
    ));
}

#[test]
fn unsubscribe_rejects_empty_topic_list() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let result = block_on(session.unsubscribe(&[], &[]));
    assert!(matches!(
        result,
        Err(Error::Protocol(ProtocolError::NoTopic))
    ));
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
        block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::ExactlyOnce))).unwrap();
    }

    for _ in 0..4 {
        assert!(poll_now(&mut session).unwrap().is_none());
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

    with_inbound(&mut session, |message| {
        assert_eq!(message.topic(), "data");
    });

    assert!(poll_now(&mut session).unwrap().is_none());
}

#[test]
fn resumed_session_suppresses_duplicate_inbound_qos2_publish() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    let second_inspect = second.clone();
    second.push_rx(&connack_session_present());
    second.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    second.push_rx(&pubrel(9));

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    with_inbound(&mut session, |message| assert_eq!(message.topic(), "data"));

    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    wait_for_tx_frame(&mut session, &second_inspect, &outbound_pubcomp(9, 0x00));

    let tx = second_inspect.tx();
    assert_eq!(
        tx.iter()
            .filter(|frame| frame.as_slice() == outbound_pubrec(9))
            .count(),
        1
    );
    assert!(
        tx.iter()
            .any(|frame| frame.as_slice() == outbound_pubcomp(9, 0x00))
    );
}

#[test]
fn fresh_session_clears_pending_inbound_qos2_state() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    let second_inspect = second.clone();
    second.push_rx(&connack());
    second.push_rx(&pubrel(9));

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    with_inbound(&mut session, |message| assert_eq!(message.topic(), "data"));

    expect_disconnected(&mut session);
    expect_connected(&mut session, &connector);
    wait_for_tx_frame(&mut session, &second_inspect, &outbound_pubcomp(9, 0x92));

    assert!(
        second_inspect
            .tx()
            .iter()
            .any(|frame| frame.as_slice() == outbound_pubcomp(9, 0x92))
    );
}

#[test]
fn full_retained_outbound_still_sends_puback() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos1(9, "data", b"x"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let capacity = fill_inflight_publish_slots(&mut session);
    assert!(capacity > 0);

    with_inbound(&mut session, |message| assert_eq!(message.topic(), "data"));

    wait_for_tx_frame(&mut session, &inspect, &outbound_puback(9));
}

#[test]
fn full_retained_outbound_still_sends_pubrec() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    let capacity = fill_inflight_publish_slots(&mut session);
    assert!(capacity > 0);

    with_inbound(&mut session, |message| assert_eq!(message.topic(), "data"));

    wait_for_tx_frame(&mut session, &inspect, &outbound_pubrec(9));
}

#[test]
fn connack_receive_maximum_clamps_local_quota() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack_receive_max(2));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();

    let result = block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce)));
    assert!(matches!(result, Err(PubError::Session(Error::NotReady))));
}

#[test]
fn session_allows_publish_after_message_borrow_is_dropped() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos1(7, "cmd", b"payload"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    {
        with_inbound(&mut session, |message| {
            assert_eq!(message.topic(), "cmd");
            assert_eq!(message.payload(), b"payload");
        });
    }

    block_on(session.publish(Publication::bytes("reply", b"ok").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn session_reconnects_after_write_error() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.fail_write_after(1, ErrorKind::ConnectionReset);

    let mut second = MockConnection::default();
    second.push_rx(&connack_session_present());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    let error = block_on(session.publish(Publication::bytes("reply", b"ok").qos(QoS::AtLeastOnce)))
        .unwrap_err();
    assert!(matches!(
        error,
        PubError::Session(Error::Transport(ErrorKind::ConnectionReset))
    ));

    expect_reconnected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("reply", b"ok").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn puback_failure_is_reported_and_clears_inflight() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&puback_reason(1, 0x87));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();
    expect_poll_error(&mut session, |err| {
        matches!(
            err,
            Error::Protocol(ProtocolError::Failed(minimq::ReasonCode::NotAuthorized))
        )
    });
    block_on(session.publish(Publication::bytes("data", b"y").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn pubrec_failure_is_reported_and_clears_inflight() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec_reason(1, 0x97));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::ExactlyOnce))).unwrap();
    expect_poll_error(&mut session, |err| {
        matches!(
            err,
            Error::Protocol(ProtocolError::Failed(minimq::ReasonCode::QuotaExceeded))
        )
    });
    block_on(session.publish(Publication::bytes("data", b"y").qos(QoS::ExactlyOnce))).unwrap();
}

#[test]
fn pubcomp_failure_is_reported_and_clears_release_state() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec(1));
    connection.push_rx(&pubcomp_reason(1, 0x83));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::ExactlyOnce))).unwrap();
    expect_poll_error(&mut session, |err| {
        matches!(
            err,
            Error::Protocol(ProtocolError::Failed(
                minimq::ReasonCode::ImplementationError
            ))
        )
    });
    block_on(session.publish(Publication::bytes("data", b"y").qos(QoS::ExactlyOnce))).unwrap();
}

#[test]
fn poll_requires_connected_session() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = Session::new(config());

    assert!(matches!(block_on(session.poll()), Err(Error::Disconnected)));
    expect_connected(&mut session, &connector);
}

#[test]
fn connect_retries_cleanly_after_cancellation_during_pending_write() {
    let mut first = MockConnection::default();
    first.pend_writes(1);
    first.push_rx(&connack());
    let second = {
        let mut connection = MockConnection::default();
        connection.push_rx(&connack());
        connection
    };
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    let mut future = Box::pin(session.connect(connector.connect().unwrap()));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert!(!session.is_connected());
    assert!(matches!(block_on(session.poll()), Err(Error::Disconnected)));
    expect_connected(&mut session, &connector);
}

#[test]
fn connect_retries_cleanly_after_cancellation_during_pending_connack() {
    let mut first = MockConnection::default();
    first.pend_reads(1);
    first.push_rx(&connack());
    let second = {
        let mut connection = MockConnection::default();
        connection.push_rx(&connack());
        connection
    };
    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    let mut future = Box::pin(session.connect(connector.connect().unwrap()));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert!(!session.is_connected());
    assert!(matches!(block_on(session.poll()), Err(Error::Disconnected)));
    expect_connected(&mut session, &connector);
}

#[test]
fn disconnect_sends_disconnect_packet_and_drops_connection() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    block_on(session.disconnect()).unwrap();

    assert!(!session.is_connected());
    assert_eq!(inspect.tx().last().unwrap(), &disconnect_req());
}

#[test]
fn disconnect_survives_cancellation_during_pending_write() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    inspect.pend_writes(1);
    let mut future = Box::pin(session.disconnect());
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert!(session.is_connected());
    block_on(session.disconnect()).unwrap();
    assert!(!session.is_connected());
    assert_eq!(inspect.tx().last().unwrap(), &disconnect_req());
}

#[test]
fn session_reconnects_after_replay_write_error() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    second.push_rx(&connack_session_present());
    second.fail_write_after(1, ErrorKind::ConnectionReset);

    let mut third = MockConnection::default();
    third.push_rx(&connack_session_present());

    let connector = MockConnector::with_connections([first, second, third]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();

    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    assert!(matches!(
        block_on(session.poll()),
        Err(Error::Transport(ErrorKind::ConnectionReset))
    ));
    expect_reconnected(&mut session, &connector);
}

#[test]
fn qos1_publish_replays_across_multiple_resumed_reconnects() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    let second_inspect = second.clone();
    second.push_rx(&connack_session_present());
    second.push_rx(&disconnect());

    let mut third = MockConnection::default();
    let third_inspect = third.clone();
    third.push_rx(&connack_session_present());
    third.push_rx(&puback(1));

    let connector = MockConnector::with_connections([first, second, third]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();

    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut session).unwrap().is_none());

    assert!(
        second_inspect
            .tx()
            .iter()
            .any(|frame| frame.first() == Some(&0x3A))
    );
    assert!(
        third_inspect
            .tx()
            .iter()
            .any(|frame| frame.first() == Some(&0x3A))
    );
}

#[test]
fn qos2_pubrel_replays_across_multiple_resumed_reconnects() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&pubrec(1));
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    let second_inspect = second.clone();
    second.push_rx(&connack_session_present());
    second.push_rx(&disconnect());

    let mut third = MockConnection::default();
    let third_inspect = third.clone();
    third.push_rx(&connack_session_present());
    third.push_rx(&pubcomp(1));

    let connector = MockConnector::with_connections([first, second, third]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::ExactlyOnce))).unwrap();

    match poll_now(&mut session) {
        Ok(None) | Err(Error::Disconnected) => {}
        other => panic!("unexpected poll result before disconnect: {other:?}"),
    }
    if session.is_connected() {
        expect_disconnected(&mut session);
    }
    expect_reconnected(&mut session, &connector);
    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut session).unwrap().is_none());

    assert!(
        second_inspect
            .tx()
            .iter()
            .any(|frame| frame.as_slice() == outbound_pubrel(1))
    );
    assert!(
        third_inspect
            .tx()
            .iter()
            .any(|frame| frame.as_slice() == outbound_pubrel(1))
    );
}

#[test]
fn fresh_session_after_reconnect_drops_stale_replay_state() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    let second_inspect = second.clone();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();

    expect_disconnected(&mut session);
    expect_connected(&mut session, &connector);
    assert!(poll_now(&mut session).unwrap().is_none());

    assert!(
        second_inspect
            .tx()
            .iter()
            .all(|frame| frame.first() != Some(&0x3A))
    );
}

#[test]
fn fresh_session_failed_connack_still_drops_stale_replay_state() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    second.push_rx(&connack_receive_max(0));

    let mut third = MockConnection::default();
    let third_inspect = third.clone();
    third.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second, third]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();

    expect_disconnected(&mut session);
    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Protocol(ProtocolError::InvalidProperty))
    ));
    expect_connected(&mut session, &connector);
    assert!(poll_now(&mut session).unwrap().is_none());

    assert!(
        third_inspect
            .tx()
            .iter()
            .all(|frame| frame.first() != Some(&0x3A))
    );
}

#[test]
fn stale_puback_after_resumed_replay_is_ignored() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    second.push_rx(&connack_session_present());
    second.push_rx(&puback(1));
    second.push_rx(&puback(1));

    let connector = MockConnector::with_connections([first, second]);
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce))).unwrap();

    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut session).unwrap().is_none());
    assert!(poll_now(&mut session).unwrap().is_none());
}

#[test]
fn connect_uses_configured_session_expiry_interval() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = Session::new(
        ConfigBuilder::new(Buffers::new(
            Box::leak(Box::new([0; 128])),
            Box::leak(Box::new([0; 1152])),
        ))
        .client_id("test")
        .unwrap()
        .session_expiry_interval(7),
    );

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())).unwrap(),
        ConnectEvent::Connected
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

    let result = block_on(session.publish(Publication::bytes("data", b"x")));
    assert!(matches!(
        result,
        Err(PubError::Session(Error::Protocol(ProtocolError::Failed(
            minimq::ReasonCode::PacketTooLarge
        ))))
    ));
    assert_eq!(inspect.tx().len(), 1);
}

#[test]
fn oversized_required_ack_disconnects_session() {
    let mut first = MockConnection::default();
    first.push_rx(&connack_max_packet_size(4));
    first.push_rx(&inbound_publish_qos1(9, "data", b"x"));

    let mut second = MockConnection::default();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = connected_session(&connector);

    expect_poll_error(&mut session, |err| {
        matches!(
            err,
            Error::Protocol(ProtocolError::Failed(minimq::ReasonCode::PacketTooLarge))
        )
    });
    expect_connected(&mut session, &connector);
}

#[test]
fn successful_required_ack_fits_small_broker_maximum_packet_size() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_packet_size(5));
    connection.push_rx(&inbound_publish_qos1(9, "data", b"x"));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);

    with_inbound(&mut session, |message| {
        assert_eq!(message.topic(), "data");
        assert_eq!(message.payload(), b"x");
    });
    wait_for_tx_frame(&mut session, &inspect, &outbound_puback(9));
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
    let mut session = Session::new(config());

    expect_connected(&mut session, &connector);
    block_on(session.unsubscribe(&["data"], &[])).unwrap();
    expect_disconnected(&mut session);
    expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut session).unwrap().is_none());

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
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_with_response(
        "cmd",
        b"payload",
        "reply/topic",
        b"abc",
    ));
    let connector = MockConnector::new(connection);
    let mut session = connected_session(&connector);
    with_inbound(&mut session, |message| {
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
    });
}
