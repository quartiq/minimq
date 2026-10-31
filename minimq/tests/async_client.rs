mod support;

use embassy_time::{Duration, with_timeout};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use minimq::{
    Buffers, ConfigBuilder, ConnectEvent, Connection, Disconnect, Error, InboundPublish, Op,
    PeerError, Property, PubError, Publication, QoS, ReasonCode, ResourceError, Session,
    TopicFilter,
};

/// A live connection handle over the mock transport, returned by `Session::connect`.
type Conn<'a> = Connection<'a, 'static, MockConnection>;
use std::{cell::RefCell, collections::VecDeque, future::poll_fn, rc::Rc, task::Poll};
use support::{block_on, init_host_logging, poll_once};

const WAIT_STEPS: usize = 64;
const QOS1_DUP_PUBLISH_HEADER: u8 = 0x3a;

const V3: minimq::Protocol = minimq::Protocol::V3_1_1 {
    clean_session: false,
};

#[test]
fn zero_publish_identifier_disconnects_before_delivery() {
    for protocol in [minimq::Protocol::V5, V3] {
        for header in [0x32, 0x34] {
            let mut session = Session::new(config().protocol(protocol));
            let mut io = MockConnection::default();
            if protocol == V3 {
                io.push_rx(&[0x20, 2, 0, 0]);
            } else {
                io.push_rx(&[0x20, 3, 0, 0, 0]);
            }
            let mut conn = block_on(session.connect(io.clone())).unwrap();
            io.inner.borrow_mut().tx.clear();
            if protocol == V3 {
                io.push_rx_fragmented(&[header, 6, 0, 1, b'a', 0, 0, b'x']);
            } else {
                io.push_rx_fragmented(&[header, 7, 0, 1, b'a', 0, 0, 0, b'x']);
            }
            assert!(matches!(
                block_on(conn.recv()),
                Err(Error::Peer(PeerError::InvalidPacket))
            ));
            assert!(!conn.is_connected());
            assert!(io.tx().is_empty());
        }
    }
}

#[test]
fn v3_inbound_qos2_exhaustion_closes_without_acknowledging() {
    let mut session = Session::new(config().protocol(V3));
    let mut io = MockConnection::default();
    io.push_rx(&[0x20, 2, 0, 0]);
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    for id in 1..=u8::MAX {
        io.push_rx(&[0x34, 6, 0, 1, b'a', 0, id, b'x']);
        match block_on(conn.recv()) {
            Ok(_) => wait_for_tx_frame(&mut conn, &io, &[0x50, 2, 0, id]),
            Err(Error::Peer(PeerError::InvalidPacket)) => {
                assert!(!conn.is_connected());
                assert!(!io.tx().iter().any(|packet| packet == &[0x50, 2, 0, id]));
                return;
            }
            Err(error) => panic!("unexpected error: {error:?}"),
        }
    }
    panic!("expected bounded inbound QoS 2 capacity");
}

#[test]
fn v3_small_asymmetric_buffers_and_invalid_connack() {
    for tx_len in [20, 21, 64] {
        let mut rx = [0; 4];
        let mut tx = [0; 64];
        let mut session = Session::new(
            ConfigBuilder::new(Buffers::new(&mut rx, &mut tx[..tx_len]))
                .client_id("test")
                .unwrap()
                .protocol(V3),
        );
        let mut io = MockConnection::default();
        io.push_rx_fragmented(&[0x20, 2, 0, 0]);
        io.inner.borrow_mut().write_limit = Some(1);
        let result = block_on(session.connect(io));
        assert_eq!(result.is_ok(), tx_len >= 21);
    }
    for ack in [
        &[0x20, 2, 2, 0][..],
        &[0x20, 2, 1, 1],
        &[0x20, 2, 0, 6],
        &[0x20, 3, 0, 0, 0],
        &[0x20, 2, 1, 0],
    ] {
        let mut session = Session::new(config().protocol(minimq::Protocol::V3_1_1 {
            clean_session: true,
        }));
        let mut io = MockConnection::default();
        io.push_rx(ack);
        assert!(block_on(session.connect(io)).is_err());
    }
}

#[test]
fn v3_wire_connect_auth_will_and_refusal() {
    for (code, reason) in [
        (1, ReasonCode::UnsupportedProtocol),
        (2, ReasonCode::ClientIdentifierInvalid),
        (3, ReasonCode::ServerUnavailable),
        (4, ReasonCode::BadUsernameOrPassword),
        (5, ReasonCode::NotAuthorized),
    ] {
        let mut session = Session::new(
            config()
                .protocol(V3)
                .auth("u", b"p")
                .unwrap()
                .will(
                    minimq::Will::new("w", b"x", &[])
                        .unwrap()
                        .qos(QoS::ExactlyOnce)
                        .retained(),
                )
                .unwrap(),
        );
        let mut io = MockConnection::default();
        io.push_rx_fragmented(&[0x20, 2, 0, code]);
        assert!(
            matches!(block_on(session.connect(io.clone())), Err(Error::Peer(PeerError::Rejected(got))) if got == reason)
        );
        assert_eq!(
            io.tx().concat(),
            b"\x10\x1c\x00\x04MQTT\x04\xf4\x00\x3c\x00\x04test\x00\x01w\x00\x01x\x00\x01u\x00\x01p"
        );
    }
}

#[test]
fn v3_wire_publish_subscribe_unsubscribe_and_qos() {
    let mut session = Session::new(config().protocol(V3));
    let mut io = MockConnection::default();
    io.push_rx_fragmented(&[0x20, 2, 0, 0]);
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    io.inner.borrow_mut().tx.clear();
    block_on(conn.publish(Publication::new("a", b"x".as_slice()))).unwrap();
    assert_eq!(io.tx().concat(), b"\x30\x04\x00\x01ax");
    for (qos, header, id) in [(QoS::AtLeastOnce, 0x32, 1), (QoS::ExactlyOnce, 0x34, 2)] {
        io.inner.borrow_mut().tx.clear();
        let op = block_on(conn.publish(Publication::new("a", b"x".as_slice()).qos(qos)))
            .unwrap()
            .unwrap();
        assert_eq!(io.tx().concat(), [header, 6, 0, 1, b'a', 0, id, b'x']);
        if qos == QoS::ExactlyOnce {
            io.push_rx(&pubrec(id as u16));
            wait_for_tx_frame(&mut conn, &io, &[0x62, 2, 0, id]);
            io.push_rx(&pubcomp(id as u16));
        } else {
            io.push_rx(&puback(id as u16));
        }
        for _ in 0..WAIT_STEPS {
            let _ = poll_now(&mut conn).unwrap();
            if conn.is_complete(&op) {
                break;
            }
        }
        assert!(conn.is_complete(&op));
    }
    io.inner.borrow_mut().tx.clear();
    let op = block_on(conn.subscribe(&[TopicFilter::new("a")], &[])).unwrap();
    assert_eq!(io.tx().concat(), b"\x82\x06\x00\x03\x00\x01a\x00");
    io.push_rx(&[0x90, 3, 0, 3, 0]);
    for _ in 0..WAIT_STEPS {
        let _ = poll_now(&mut conn).unwrap();
        if conn.is_complete(&op) {
            break;
        }
    }
    assert!(conn.is_complete(&op));
    io.inner.borrow_mut().tx.clear();
    let op = block_on(conn.unsubscribe(&["a"], &[])).unwrap();
    assert_eq!(io.tx().concat(), b"\xa2\x05\x00\x04\x00\x01a");
    io.push_rx(&[0xb0, 2, 0, 4]);
    for _ in 0..WAIT_STEPS {
        let _ = poll_now(&mut conn).unwrap();
        if conn.is_complete(&op) {
            break;
        }
    }
    assert!(conn.is_complete(&op));
    for (header, id, ack) in [(0x32, 7, 0x40), (0x34, 8, 0x50)] {
        io.push_rx_fragmented(&[header, 6, 0, 1, b'a', 0, id, 0]);
        with_inbound(&mut conn, |incoming| {
            assert_eq!(incoming.payload(), &[0]);
        });
        wait_for_tx_frame(&mut conn, &io, &[ack, 2, 0, id]);
    }
    io.push_rx(&[0x62, 2, 0, 8]);
    wait_for_tx_frame(&mut conn, &io, &[0x70, 2, 0, 8]);
    // An unknown/repeated PUBREL still gets a reason-free PUBCOMP in 3.1.1.
    io.push_rx(&[0x62, 2, 0, 9]);
    wait_for_tx_frame(&mut conn, &io, &[0x70, 2, 0, 9]);
    block_on(conn.disconnect()).unwrap();
    assert_eq!(io.tx().last().unwrap(), &[0xe0, 0]);
}

#[test]
fn v3_cancelled_publish_replays_only_when_broker_resumes() {
    for clean_session in [false, true] {
        for resumed in [false, true] {
            if clean_session && resumed {
                continue;
            }
            let mut session =
                Session::new(config().protocol(minimq::Protocol::V3_1_1 { clean_session }));
            let mut io = MockConnection::default();
            io.push_rx(&[0x20, 2, 0, 0]);
            let mut conn = block_on(session.connect(io.clone())).unwrap();
            assert_eq!(io.tx().concat()[9], if clean_session { 2 } else { 0 });
            io.inner.borrow_mut().write_limit = Some(1);
            io.pend_flushes(1);
            let mut publish = Box::pin(
                conn.publish(Publication::new("a", b"x".as_slice()).qos(QoS::AtLeastOnce)),
            );
            assert!(poll_once(publish.as_mut()).is_pending());
            drop(publish);
            drop(conn);
            let mut io = MockConnection::default();
            io.push_rx_fragmented(&[0x20, 2, u8::from(resumed), 0]);
            let mut conn = block_on(session.connect(io.clone())).unwrap();
            assert_eq!(
                conn.connect_event(),
                if resumed {
                    ConnectEvent::Reconnected
                } else {
                    ConnectEvent::Connected
                }
            );
            assert_eq!(io.tx().concat()[9], if clean_session { 2 } else { 0 });
            if resumed {
                wait_for_tx_frame(&mut conn, &io, b"\x3a\x06\x00\x01a\x00\x01x");
            } else {
                assert!(conn.session().is_publish_quiescent());
            }
        }
    }
}

#[test]
fn v3_rejects_v5_options_before_writing() {
    for config in [
        config().protocol(V3).session_expiry_interval(0),
        config().protocol(V3).session_expiry_interval(60),
        config().protocol(V3).client_id("").unwrap(),
        config()
            .protocol(V3)
            .will(minimq::Will::new("w", b"x", &[Property::ContentType("x")]).unwrap())
            .unwrap(),
    ] {
        let mut session = Session::new(config);
        let io = MockConnection::default();
        assert!(matches!(
            block_on(session.connect(io.clone())),
            Err(Error::InvalidRequest)
        ));
        assert!(io.tx().is_empty());
    }
    let mut session = Session::new(config().protocol(V3));
    let mut io = MockConnection::default();
    io.push_rx(&[0x20, 2, 0, 0]);
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    io.inner.borrow_mut().tx.clear();
    assert!(matches!(
        block_on(conn.publish(
            Publication::new("a", b"x".as_slice()).properties(&[Property::ContentType("x")])
        )),
        Err(PubError::Session(Error::InvalidRequest))
    ));
    assert!(matches!(
        block_on(
            conn.subscribe(
                &[TopicFilter::new("a")
                    .options(minimq::SubscriptionOptions::default().ignore_local_messages())],
                &[]
            )
        ),
        Err(Error::InvalidRequest)
    ));
    assert!(matches!(
        block_on(conn.unsubscribe(&["a"], &[Property::UserProperty("x", "y")])),
        Err(Error::InvalidRequest)
    ));
    assert!(matches!(
        block_on(conn.disconnect_with(Disconnect::with_will())),
        Err(Error::InvalidRequest)
    ));
    assert!(io.tx().is_empty());
}

#[derive(Default)]
struct MockIo {
    rx: VecDeque<Vec<u8>>,
    tx: Vec<Vec<u8>>,
    read_error: Option<ErrorKind>,
    write_error: Option<(usize, ErrorKind)>,
    write_zero: Option<usize>,
    pending_reads: usize,
    pending_writes: usize,
    pending_flushes: usize,
    buffered: Vec<u8>,
    flush_groups: Vec<Vec<u8>>,
    flush_error: Option<ErrorKind>,
    write_limit: Option<usize>,
    buffer_capacity: Option<usize>,
    automatic_flushes: usize,
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

    fn zero_write_after(&mut self, successful_writes: usize) {
        self.inner.borrow_mut().write_zero = Some(successful_writes);
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
            if let Some(remaining) = &mut inner.write_zero {
                if *remaining == 0 {
                    inner.write_zero = None;
                    return Poll::Ready(Ok(0));
                }
                *remaining -= 1;
            }
            // Model a streaming buffered transport: a full buffer emits data without flush().
            if inner.buffer_capacity == Some(inner.buffered.len()) {
                inner.buffered.clear();
                inner.automatic_flushes += 1;
            }
            let count = buf.len().min(inner.write_limit.unwrap_or(usize::MAX)).min(
                inner
                    .buffer_capacity
                    .map_or(usize::MAX, |capacity| capacity - inner.buffered.len()),
            );
            inner.tx.push(buf[..count].to_vec());
            inner.buffered.extend_from_slice(&buf[..count]);
            Poll::Ready(Ok(count))
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
            if let Some(error) = inner.flush_error.take() {
                return Poll::Ready(Err(error));
            }
            let bytes = std::mem::take(&mut inner.buffered);
            inner.flush_groups.push(bytes);
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
    init_host_logging();
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

fn connack_max_qos(max: QoS) -> [u8; 7] {
    [0x20, 0x05, 0x00, 0x00, 0x02, 0x24, max as u8]
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

fn disconnect_with_will() -> [u8; 3] {
    [0xE0, 0x01, ReasonCode::DisconnectWithWill as u8]
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

fn outbound_puback(id: u16) -> [u8; 4] {
    [0x40, 0x02, (id >> 8) as u8, id as u8]
}

fn outbound_pubrec(id: u16) -> [u8; 4] {
    [0x50, 0x02, (id >> 8) as u8, id as u8]
}

fn outbound_pubrel(id: u16) -> [u8; 4] {
    [0x62, 0x02, (id >> 8) as u8, id as u8]
}

fn outbound_pubcomp(id: u16, reason: u8) -> Vec<u8> {
    if reason == 0 {
        vec![0x70, 0x02, (id >> 8) as u8, id as u8]
    } else {
        vec![0x70, 0x03, (id >> 8) as u8, id as u8, reason]
    }
}

fn session() -> Session<'static> {
    Session::new(config())
}

fn expect_connected<'a>(session: &'a mut Session<'static>, connector: &MockConnector) -> Conn<'a> {
    let conn = block_on(session.connect(connector.connect().unwrap())).unwrap();
    assert!(matches!(conn.connect_event(), ConnectEvent::Connected));
    conn
}

fn expect_reconnected<'a>(
    session: &'a mut Session<'static>,
    connector: &MockConnector,
) -> Conn<'a> {
    let conn = block_on(session.connect(connector.connect().unwrap())).unwrap();
    assert!(matches!(conn.connect_event(), ConnectEvent::Reconnected));
    conn
}

fn poll_now<'a>(conn: &'a mut Conn<'_>) -> Result<Option<InboundPublish<'a>>, Error<ErrorKind>> {
    match block_on(with_timeout(Duration::from_millis(0), conn.poll())) {
        Ok(Ok(event)) => Ok(event),
        Ok(Err(err)) => Err(err),
        Err(_) => Ok(None),
    }
}

fn expect_disconnected(conn: &mut Conn<'_>) {
    for _ in 0..WAIT_STEPS {
        match poll_now(conn) {
            Err(Error::Disconnected) => return,
            Ok(None) => {}
            other => panic!("unexpected poll result while waiting for disconnect: {other:?}"),
        }
    }
    panic!("timed out waiting for disconnect");
}

fn with_inbound<T>(conn: &mut Conn<'_>, f: impl FnOnce(InboundPublish<'_>) -> T) -> T {
    f(block_on(conn.recv()).unwrap())
}

fn expect_poll_error(conn: &mut Conn<'_>, want: impl Fn(&Error<ErrorKind>) -> bool) {
    for _ in 0..WAIT_STEPS {
        match poll_now(conn) {
            Ok(None) => {}
            Err(err) if want(&err) => return,
            other => panic!("unexpected poll result while waiting for error: {other:?}"),
        }
    }
    panic!("timed out waiting for poll error");
}

fn wait_for_tx_frame(conn: &mut Conn<'_>, inspect: &MockConnection, expected: &[u8]) {
    if inspect
        .tx()
        .iter()
        .any(|frame| frame.as_slice() == expected)
    {
        return;
    }

    for _ in 0..WAIT_STEPS {
        match poll_now(conn) {
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

fn fill_inflight_publish_slots(conn: &mut Conn<'_>) -> usize {
    let mut count = 0;
    loop {
        match publish_qos1(conn, "data", b"x") {
            Ok(Some(_)) => count += 1,
            Ok(None) => unreachable!("QoS1 publish must return an op handle"),
            Err(PubError::Session(Error::Resource(ResourceError::InflightExhausted))) => {
                return count;
            }
            other => panic!("unexpected publish result while filling inflight slots: {other:?}"),
        }
    }
}

fn publish_qos1(
    conn: &mut Conn<'_>,
    topic: &str,
    payload: &[u8],
) -> Result<Option<Op>, PubError<(), ErrorKind>> {
    block_on(conn.publish(Publication::bytes(topic, payload).qos(QoS::AtLeastOnce)))
}

fn publish_qos1_ok(conn: &mut Conn<'_>, topic: &str, payload: &[u8]) {
    publish_qos1(conn, topic, payload).unwrap();
}

fn publish_qos2(
    conn: &mut Conn<'_>,
    topic: &str,
    payload: &[u8],
) -> Result<Option<Op>, PubError<(), ErrorKind>> {
    block_on(conn.publish(Publication::bytes(topic, payload).qos(QoS::ExactlyOnce)))
}

fn publish_qos2_ok(conn: &mut Conn<'_>, topic: &str, payload: &[u8]) {
    publish_qos2(conn, topic, payload).unwrap();
}

#[test]
fn fragmented_connack_still_establishes_session() {
    let mut connection = MockConnection::default();
    connection.push_rx_fragmented(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();

    let _conn = expect_connected(&mut session, &connector);
}

#[test]
fn fragmented_inbound_publish_is_assembled_and_acked() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx_fragmented(&inbound_publish_qos1(7, "data", b"payload"));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    with_inbound(&mut conn, |message| {
        assert_eq!(message.topic(), "data");
        assert_eq!(message.payload(), b"payload");
    });

    wait_for_tx_frame(&mut conn, &inspect, &outbound_puback(7));
}

#[test]
fn publish_requires_connected_session() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();

    // With the handle-based API, a publish is only reachable through a `Connection`,
    // so "publish requires a connected session" is now a compile-time guarantee:
    // there is no `Session::publish` to call before `connect`. Verify publishing
    // works once connected.
    let mut conn = expect_connected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "data", b"payload");
}

#[test]
fn publish_survives_cancellation_during_pending_write() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&puback(1));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    inspect.pend_writes(1);
    let mut future = Box::pin(conn.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert_eq!(inspect.tx().len(), 1);
    assert!(poll_now(&mut conn).unwrap().is_none());
    publish_qos1_ok(&mut conn, "data", b"y");
}

#[test]
fn cancelled_publish_still_holds_receive_max_quota() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack_receive_max(1));
    connection.push_rx(&puback(1));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    inspect.pend_writes(1);
    let mut future = Box::pin(conn.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce)));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    let result = publish_qos1(&mut conn, "data", b"y");
    assert!(matches!(result, Err(PubError::Session(Error::NotReady))));
    for _ in 0..WAIT_STEPS {
        match publish_qos1(&mut conn, "data", b"z") {
            Ok(Some(_)) => return,
            Ok(None) => unreachable!("QoS1 publish must return an op handle"),
            Err(PubError::Session(Error::NotReady)) => {
                assert!(poll_now(&mut conn).unwrap().is_none());
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
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    inspect.pend_flushes(1);
    let topics = [TopicFilter::new("data")];
    let mut future = Box::pin(conn.subscribe(&topics, &[]));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert_eq!(inspect.tx().len(), 2);
    assert!(poll_now(&mut conn).unwrap().is_none());
    assert_eq!(inspect.tx().len(), 2);
}

#[test]
fn poll_survives_cancellation_during_pending_read() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    inspect.pend_reads(1);
    let mut future = Box::pin(conn.poll());
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    assert!(poll_now(&mut conn).unwrap().is_none());
}

#[test]
fn qos2_pubrel_survives_cancellation_during_pending_write() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec(1));
    connection.push_rx(&pubcomp(1));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    publish_qos2_ok(&mut conn, "data", b"x");
    assert!(poll_now(&mut conn).unwrap().is_none());

    inspect.pend_writes(1);
    let mut sent_pubrel = false;
    for _ in 0..WAIT_STEPS {
        match poll_now(&mut conn).unwrap() {
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
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    publish_qos1_ok(&mut conn, "data", b"x");
    let tx_after_publish = connector.connections.borrow().front().is_none();
    assert!(tx_after_publish);

    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(poll_now(&mut conn).unwrap().is_none());
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "data", b"x");
    expect_disconnected(&mut conn);
    drop(conn);
    let _conn = expect_reconnected(&mut session, &connector);
}

#[test]
fn broker_disconnect_without_session_resume_reports_connected() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.push_rx(&disconnect());

    let mut second = MockConnection::default();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    expect_disconnected(&mut conn);
    drop(conn);
    let _conn = expect_connected(&mut session, &connector);
}

#[test]
fn handle_latches_disconnect_and_fails_fast() {
    // A disconnect observed *while the user still holds the handle* must be latched, so
    // `is_connected()` reflects it and further operations fail fast instead of driving the dead
    // transport.
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&disconnect());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    assert!(conn.is_connected());
    expect_disconnected(&mut conn);

    // The broker DISCONNECT is now latched on the handle.
    assert!(!conn.is_connected());
    assert!(matches!(block_on(conn.poll()), Err(Error::Disconnected)));
    assert!(matches!(
        publish_qos1(&mut conn, "data", b"x"),
        Err(PubError::Session(Error::Disconnected))
    ));
}

#[test]
fn dropping_a_live_connection_sends_no_disconnect_and_resumes() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    let first_inspect = first.clone();
    let mut second = MockConnection::default();
    second.push_rx(&connack_session_present());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    // A healthy, connected session dropped WITHOUT a graceful disconnect.
    let conn = expect_connected(&mut session, &connector);
    drop(conn);

    // A bare drop is an ungraceful MQTT close: no DISCONNECT (0xE0) packet is written.
    assert!(
        !first_inspect
            .tx()
            .iter()
            .any(|frame| frame.first() == Some(&0xE0)),
        "dropping the handle must not emit a DISCONNECT packet"
    );

    // Reconnecting resumes the existing broker session.
    let _conn = expect_reconnected(&mut session, &connector);
}

#[test]
fn failed_connack_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&[0x20, 0x03, 0x00, 0x87, 0x00]);
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Peer(PeerError::Rejected(ReasonCode::NotAuthorized)))
    ));
    let _conn = expect_connected(&mut session, &connector);
}

#[test]
fn broker_disconnect_during_connect_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&disconnect());
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Disconnected)
    ));
    let _conn = expect_connected(&mut session, &connector);
}

#[test]
fn zero_receive_maximum_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&connack_receive_max(0));
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Peer(PeerError::InvalidPacket))
    ));
    expect_connected(&mut session, &connector);
}

#[test]
fn oversized_assigned_client_id_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.push_rx(&connack_assigned_client_id(&"a".repeat(65)));
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Peer(PeerError::InvalidPacket))
    ));
    expect_connected(&mut session, &connector);
}

#[test]
fn timed_out_connack_disconnects_session_and_allows_reconnect() {
    let mut first = MockConnection::default();
    first.fail_read(ErrorKind::TimedOut);
    let mut second = MockConnection::default();
    second.push_rx(&connack());
    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Transport(ErrorKind::TimedOut))
    ));
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    expect_poll_error(&mut conn, |err| {
        matches!(err, Error::Peer(PeerError::InvalidPacket))
    });
    drop(conn);
    expect_connected(&mut session, &connector);
}

#[test]
fn inflight_metadata_exhaustion_is_reported() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let capacity = fill_inflight_publish_slots(&mut conn);
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
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    for _ in 0..3 {
        publish_qos1_ok(&mut conn, "data", b"x");
    }
    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(poll_now(&mut conn).unwrap().is_none());
}

#[test]
fn acknowledgement_kind_must_match_operation() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&puback(1));
    connection.push_rx(&suback(1, 0x00));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let op = block_on(conn.subscribe(&[TopicFilter::new("data")], &[])).unwrap();
    assert!(conn.is_pending(&op));

    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(conn.is_pending(&op));

    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(conn.is_complete(&op));
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    block_on(conn.subscribe(&[TopicFilter::new("data")], &[])).unwrap();
    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    wait_for_tx_frame(
        &mut conn,
        &inspect,
        &[
            0x82, 0x0A, 0x00, 0x01, 0x00, 0x00, 0x04, b'd', b'a', b't', b'a', 0x00,
        ],
    );

    assert_eq!(
        inspect.tx().last().unwrap(),
        &vec![
            0x82, 0x0A, 0x00, 0x01, 0x00, 0x00, 0x04, b'd', b'a', b't', b'a', 0x00
        ]
    );
}

#[test]
fn subscribe_reports_inflight_metadata_exhaustion_before_send() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let capacity = fill_inflight_publish_slots(&mut conn);

    let result = block_on(conn.subscribe(&[TopicFilter::new("data")], &[]));
    assert!(matches!(
        result,
        Err(Error::Resource(ResourceError::InflightExhausted))
    ));
    assert_eq!(inspect.tx().len(), capacity + 1);
}

#[test]
fn subscribe_rejects_empty_topic_list() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    assert!(matches!(
        block_on(conn.subscribe(&[], &[])),
        Err(Error::InvalidRequest)
    ));
}

#[test]
fn unsubscribe_rejects_empty_topic_list() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    assert!(matches!(
        block_on(conn.unsubscribe(&[], &[])),
        Err(Error::InvalidRequest)
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
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    for _ in 0..2 {
        publish_qos2_ok(&mut conn, "data", b"x");
    }

    for _ in 0..4 {
        assert!(poll_now(&mut conn).unwrap().is_none());
    }
}

#[test]
fn inbound_qos2_marks_pending_until_pubrel() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    connection.push_rx(&pubrel(9));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    with_inbound(&mut conn, |message| {
        assert_eq!(message.topic(), "data");
    });

    assert!(poll_now(&mut conn).unwrap().is_none());
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    with_inbound(&mut conn, |message| assert_eq!(message.topic(), "data"));

    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    wait_for_tx_frame(&mut conn, &second_inspect, &outbound_pubcomp(9, 0x00));

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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    with_inbound(&mut conn, |message| assert_eq!(message.topic(), "data"));

    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_connected(&mut session, &connector);
    wait_for_tx_frame(&mut conn, &second_inspect, &outbound_pubcomp(9, 0x92));

    assert!(
        second_inspect
            .tx()
            .iter()
            .any(|frame| frame.as_slice() == outbound_pubcomp(9, 0x92))
    );
}

#[test]
fn rejected_publish_drains_queued_puback() {
    let oversized = [0; 1152];
    for (ack, inflight, payload, expected) in [
        (
            connack().to_vec(),
            None,
            b"x".as_slice(),
            PubError::Session(Error::Resource(ResourceError::InflightExhausted)),
        ),
        (
            connack_receive_max(2).to_vec(),
            Some(2),
            b"x".as_slice(),
            PubError::Session(Error::NotReady),
        ),
        (
            connack().to_vec(),
            Some(0),
            oversized.as_slice(),
            PubError::Payload(()),
        ),
        (
            connack_max_packet_size(8).to_vec(),
            Some(0),
            b"x".as_slice(),
            PubError::Session(Error::Resource(ResourceError::PacketTooLarge)),
        ),
    ] {
        let mut connection = MockConnection::default();
        let inspect = connection.clone();
        connection.push_rx(&ack);
        connection.push_rx(&inbound_publish_qos1(9, "data", b"x"));
        let connector = MockConnector::new(connection);
        let mut session = session();
        let mut conn = expect_connected(&mut session, &connector);

        if let Some(count) = inflight {
            for _ in 0..count {
                publish_qos1_ok(&mut conn, "data", b"x");
            }
        } else {
            assert!(fill_inflight_publish_slots(&mut conn) > 0);
        }
        with_inbound(&mut conn, |message| assert_eq!(message.topic(), "data"));
        let before = inspect.tx();
        assert!(!before.iter().any(|frame| frame == &outbound_puback(9)));

        assert_eq!(publish_qos1(&mut conn, "data", payload), Err(expected));
        assert_eq!(
            &inspect.tx()[before.len()..],
            &[outbound_puback(9).to_vec()]
        );
        assert_eq!(
            inspect.inner.borrow().flush_groups.last().unwrap(),
            &outbound_puback(9)
        );
    }
}

#[test]
fn full_retained_outbound_still_sends_pubrec() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let capacity = fill_inflight_publish_slots(&mut conn);
    assert!(capacity > 0);

    with_inbound(&mut conn, |message| assert_eq!(message.topic(), "data"));

    wait_for_tx_frame(&mut conn, &inspect, &outbound_pubrec(9));
}

#[test]
fn connack_receive_maximum_clamps_local_quota() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack_receive_max(2));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    publish_qos1_ok(&mut conn, "data", b"x");
    publish_qos1_ok(&mut conn, "data", b"x");

    let result = publish_qos1(&mut conn, "data", b"x");
    assert!(matches!(result, Err(PubError::Session(Error::NotReady))));
}

#[test]
fn publish_above_broker_maximum_qos_fails_before_send() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_qos(QoS::AtMostOnce));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    assert!(matches!(
        publish_qos1(&mut conn, "data", b"x"),
        Err(PubError::Session(Error::InvalidRequest))
    ));
    assert_eq!(inspect.tx().len(), 1);
}

#[test]
fn publish_can_downgrade_to_broker_maximum_qos() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_qos(QoS::AtMostOnce));
    let connector = MockConnector::new(connection);
    let mut session = Session::new(config().autodowngrade_qos());
    let mut conn = expect_connected(&mut session, &connector);

    assert!(publish_qos1(&mut conn, "data", b"x").unwrap().is_none());
    assert_eq!(inspect.tx().last().unwrap()[0], 0x30);
}

#[test]
fn session_allows_publish_after_message_borrow_is_dropped() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos1(7, "cmd", b"payload"));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    {
        with_inbound(&mut conn, |message| {
            assert_eq!(message.topic(), "cmd");
            assert_eq!(message.payload(), b"payload");
        });
    }

    publish_qos1_ok(&mut conn, "reply", b"ok");
}

#[test]
fn session_reconnects_after_write_error() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.fail_write_after(1, ErrorKind::ConnectionReset);

    let mut second = MockConnection::default();
    second.push_rx(&connack_session_present());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    let error = publish_qos1(&mut conn, "reply", b"ok").unwrap_err();
    assert!(matches!(
        error,
        PubError::Session(Error::Transport(ErrorKind::ConnectionReset))
    ));
    drop(conn);

    let mut conn = expect_reconnected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "reply", b"ok");
}

#[test]
fn qos0_write_zero_kills_connection() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.zero_write_after(1);

    let mut second = MockConnection::default();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    assert!(matches!(
        block_on(conn.publish(Publication::bytes("data", b"x"))),
        Err(PubError::Session(Error::WriteZero))
    ));
    assert!(matches!(
        block_on(conn.publish(Publication::bytes("data", b"x"))),
        Err(PubError::Session(Error::Disconnected))
    ));

    drop(conn);
    expect_connected(&mut session, &connector);
}

#[test]
fn retained_write_zero_kills_connection() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.zero_write_after(1);

    let mut second = MockConnection::default();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    assert!(matches!(
        publish_qos1(&mut conn, "data", b"x"),
        Err(PubError::Session(Error::WriteZero))
    ));
    assert!(matches!(
        publish_qos1(&mut conn, "data", b"x"),
        Err(PubError::Session(Error::Disconnected))
    ));

    drop(conn);
    expect_connected(&mut session, &connector);
}

#[test]
fn puback_failure_is_reported_and_clears_inflight() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&puback_reason(1, 0x87));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    publish_qos1_ok(&mut conn, "data", b"x");
    expect_poll_error(&mut conn, |err| {
        matches!(
            err,
            Error::Peer(PeerError::Rejected(ReasonCode::NotAuthorized))
        )
    });
    publish_qos1_ok(&mut conn, "data", b"y");
}

#[test]
fn pubrec_failure_is_reported_and_clears_inflight() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec_reason(1, 0x97));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    publish_qos2_ok(&mut conn, "data", b"x");
    expect_poll_error(&mut conn, |err| {
        matches!(
            err,
            Error::Peer(PeerError::Rejected(ReasonCode::QuotaExceeded))
        )
    });
    publish_qos2_ok(&mut conn, "data", b"y");
}

#[test]
fn pubcomp_failure_is_reported_and_clears_release_state() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&pubrec(1));
    connection.push_rx(&pubcomp_reason(1, 0x83));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    publish_qos2_ok(&mut conn, "data", b"x");
    expect_poll_error(&mut conn, |err| {
        matches!(
            err,
            Error::Peer(PeerError::Rejected(ReasonCode::ImplementationError))
        )
    });
    publish_qos2_ok(&mut conn, "data", b"y");
}

#[test]
fn poll_requires_connected_session() {
    // Polling is only possible through the `Conn` handle returned by `session.connect(..)`;
    // a fresh `session()` has no handle, so there is nothing to poll. This verifies that
    // connecting yields a usable handle.
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();

    let _conn = expect_connected(&mut session, &connector);
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
    let mut session = session();

    let mut future = Box::pin(session.connect(connector.connect().unwrap()));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    // After cancellation the session is not connected (no Conn handle exists).
    // A subsequent connect with the second mock IO must succeed.
    let _conn = expect_connected(&mut session, &connector);
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
    let mut session = session();

    let mut future = Box::pin(session.connect(connector.connect().unwrap()));
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    // After cancellation the session is not connected (no Conn handle exists).
    // A subsequent connect with the second mock IO must succeed.
    let _conn = expect_connected(&mut session, &connector);
}

#[test]
fn disconnect_sends_disconnect_packet_and_marks_handle_dead() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    // `disconnect` marks the handle dead; dropping it releases the session borrow.
    block_on(conn.disconnect()).unwrap();

    assert_eq!(inspect.tx().last().unwrap(), &disconnect_req());
}

#[test]
fn disconnect_with_sends_reason_and_marks_handle_dead() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    // `disconnect_with` marks the handle dead.
    block_on(conn.disconnect_with(Disconnect::with_will())).unwrap();

    assert_eq!(inspect.tx().last().unwrap(), &disconnect_with_will());
}

#[test]
fn disconnect_uses_dedicated_control_storage_when_tx_scratch_is_full() {
    let rx = Box::leak(Box::new([0; 128]));
    let tx = Box::leak(Box::new([0; 96]));
    let config = ConfigBuilder::new(Buffers::new(rx, tx))
        .client_id("test")
        .unwrap();
    let mut session = Session::new(config);

    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut conn = expect_connected(&mut session, &connector);

    let payload = [0u8; 40];
    block_on(conn.publish(Publication::bytes("data", &payload).qos(QoS::AtLeastOnce))).unwrap();

    // `disconnect` marks the handle dead.
    block_on(conn.disconnect()).unwrap();

    assert_eq!(inspect.tx().last().unwrap(), &disconnect_req());
}

#[test]
fn outbound_operations_reject_invalid_property_contexts() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let publish_props = [Property::ServerReference("server")];
    assert_eq!(
        block_on(conn.publish(Publication::bytes("data", b"x").properties(&publish_props))),
        Err(PubError::Session(Error::InvalidRequest))
    );

    let subscribe_props = [Property::ResponseTopic("reply")];
    assert_eq!(
        block_on(conn.subscribe(&[TopicFilter::new("data")], &subscribe_props)),
        Err(Error::InvalidRequest)
    );

    let disconnect_props = [Property::PayloadFormatIndicator(1)];
    assert_eq!(
        block_on(conn.disconnect_with(Disconnect::success().with_properties(&disconnect_props))),
        Err(Error::InvalidRequest)
    );
}

#[test]
fn disconnect_retries_when_cancelled_before_writing() {
    let mut connection = MockConnection::default();
    let mut inspect = connection.clone();
    connection.push_rx(&connack());
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    inspect.pend_writes(1);
    let mut future = Box::pin(conn.disconnect());
    assert!(matches!(poll_once(future.as_mut()), Poll::Pending));
    drop(future);

    block_on(conn.disconnect()).unwrap();
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "data", b"x");

    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    assert!(matches!(
        block_on(conn.poll()),
        Err(Error::Transport(ErrorKind::ConnectionReset))
    ));
    drop(conn);
    let _conn = expect_reconnected(&mut session, &connector);
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "data", b"x");

    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut conn).unwrap().is_none());

    assert!(
        second_inspect
            .tx()
            .iter()
            .any(|frame| frame.first() == Some(&QOS1_DUP_PUBLISH_HEADER))
    );
    assert!(
        third_inspect
            .tx()
            .iter()
            .any(|frame| frame.first() == Some(&QOS1_DUP_PUBLISH_HEADER))
    );
}

#[test]
fn admitted_qos1_publish_leaves_space_to_reconnect() {
    const TX_LEN: usize = 256;
    // This payload fits the transient encode space, but not retained state plus the next CONNECT.
    const TOO_LARGE_PAYLOAD_LEN: usize = 240;
    // MQTT PUBLISH type (3), QoS 1, with and without the DUP flag.
    const PUBLISH_QOS1: u8 = 0b0011_0010;

    let mut first = MockConnection::default();
    let first_inspect = first.clone();
    first.push_rx(&connack_assigned_client_id("assigned-client"));

    let mut second = MockConnection::default();
    let second_inspect = second.clone();
    second.push_rx(&connack_session_present());

    let connector = MockConnector::with_connections([first, second]);
    let config = ConfigBuilder::new(Buffers::new(
        Box::leak(Box::new([0; 64])),
        Box::leak(Box::new([0; TX_LEN])),
    ))
    .session_expiry_interval(60);
    let mut session = Session::new(config);

    let mut conn = expect_connected(&mut session, &connector);
    assert_eq!(
        publish_qos1(&mut conn, "x", &[0; TOO_LARGE_PAYLOAD_LEN]),
        Err(PubError::Payload(()))
    );
    assert!(conn.is_connected());
    // QoS0 may still use transient space reserved against retained packets.
    block_on(conn.publish(Publication::bytes("x", &[0; TOO_LARGE_PAYLOAD_LEN]))).unwrap();
    let publication = Publication::new("x", |buffer: &mut [u8]| {
        buffer.fill(0x55);
        Ok::<_, ()>(buffer.len())
    })
    .qos(QoS::AtLeastOnce);
    block_on(conn.publish(publication)).unwrap();

    let mut replay = first_inspect.tx().last().unwrap().clone();
    assert_eq!(replay[0], PUBLISH_QOS1);
    replay[0] = QOS1_DUP_PUBLISH_HEADER;
    drop(conn);

    let mut conn = expect_reconnected(&mut session, &connector);
    wait_for_tx_frame(&mut conn, &second_inspect, &replay);
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    publish_qos2_ok(&mut conn, "data", b"x");

    let already_disconnected = match poll_now(&mut conn) {
        Ok(None) => false,
        Err(Error::Disconnected) => true,
        other => panic!("unexpected poll result before disconnect: {other:?}"),
    };
    if !already_disconnected {
        expect_disconnected(&mut conn);
    }
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut conn).unwrap().is_none());

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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "data", b"x");

    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_connected(&mut session, &connector);
    assert!(poll_now(&mut conn).unwrap().is_none());

    assert!(
        second_inspect
            .tx()
            .iter()
            .all(|frame| frame.first() != Some(&QOS1_DUP_PUBLISH_HEADER))
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "data", b"x");

    expect_disconnected(&mut conn);
    drop(conn);
    // second connect fails with InvalidPacket (zero receive_max); no conn is bound
    assert!(matches!(
        block_on(session.connect(connector.connect().unwrap())),
        Err(Error::Peer(PeerError::InvalidPacket))
    ));
    let mut conn = expect_connected(&mut session, &connector);
    assert!(poll_now(&mut conn).unwrap().is_none());

    assert!(
        third_inspect
            .tx()
            .iter()
            .all(|frame| frame.first() != Some(&QOS1_DUP_PUBLISH_HEADER))
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    publish_qos1_ok(&mut conn, "data", b"x");

    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(poll_now(&mut conn).unwrap().is_none());
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

    let conn = block_on(session.connect(connector.connect().unwrap())).unwrap();
    assert!(matches!(conn.connect_event(), ConnectEvent::Connected));
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
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let result = block_on(conn.publish(Publication::bytes("data", b"x")));
    assert!(matches!(
        result,
        Err(PubError::Session(Error::Resource(
            ResourceError::PacketTooLarge
        )))
    ));
    assert_eq!(inspect.tx().len(), 1);
}

#[test]
fn oversized_required_ack_disconnects_session() {
    let mut first = MockConnection::default();
    first.push_rx(&connack_max_packet_size(3));
    first.push_rx(&inbound_publish_qos1(9, "data", b"x"));

    let mut second = MockConnection::default();
    second.push_rx(&connack());

    let connector = MockConnector::with_connections([first, second]);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    expect_poll_error(&mut conn, |err| {
        matches!(err, Error::Resource(ResourceError::PacketTooLarge))
    });
    drop(conn);
    let _conn = expect_connected(&mut session, &connector);
}

#[test]
fn successful_required_ack_fits_small_broker_maximum_packet_size() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_packet_size(4));
    connection.push_rx(&inbound_publish_qos1(9, "data", b"x"));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    with_inbound(&mut conn, |message| {
        assert_eq!(message.topic(), "data");
        assert_eq!(message.payload(), b"x");
    });
    wait_for_tx_frame(&mut conn, &inspect, &outbound_puback(9));
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
    let mut session = session();

    let mut conn = expect_connected(&mut session, &connector);
    block_on(conn.unsubscribe(&["data"], &[])).unwrap();
    expect_disconnected(&mut conn);
    drop(conn);
    let mut conn = expect_reconnected(&mut session, &connector);
    assert!(poll_now(&mut conn).unwrap().is_none());

    assert_eq!(
        inspect.tx().last().unwrap(),
        &vec![
            0xA2, 0x09, 0x00, 0x01, 0x00, 0x00, 0x04, b'd', b'a', b't', b'a'
        ]
    );
}

#[test]
fn unsubscribe_rejects_packets_larger_than_broker_maximum() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_packet_size(8));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let result = block_on(conn.unsubscribe(&["data"], &[]));
    assert!(matches!(
        result,
        Err(Error::Resource(ResourceError::PacketTooLarge))
    ));
    assert_eq!(inspect.tx().len(), 1);
}

#[test]
fn subscribe_rejects_packets_larger_than_broker_maximum() {
    let mut connection = MockConnection::default();
    let inspect = connection.clone();
    connection.push_rx(&connack_max_packet_size(8));
    let connector = MockConnector::new(connection);
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);

    let result = block_on(conn.subscribe(&[TopicFilter::new("data")], &[]));
    assert!(matches!(
        result,
        Err(Error::Resource(ResourceError::PacketTooLarge))
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
    let mut session = session();
    let mut conn = expect_connected(&mut session, &connector);
    with_inbound(&mut conn, |message| {
        assert_eq!(message.response_topic(), Some("reply/topic"));
        assert_eq!(message.correlation_data(), Some(&b"abc"[..]));
        let owned_via_message = message.reply_owned::<64, 8>().unwrap().unwrap();
        assert_eq!(owned_via_message.topic(), "reply/topic");
        assert_eq!(owned_via_message.correlation_data(), Some(&b"abc"[..]));

        let follow_up = owned_via_message.publication("next");

        assert_eq!(owned_via_message.topic(), "reply/topic");
        let _ = message.reply("ok").unwrap().qos(QoS::AtLeastOnce);
        let _ = follow_up;
    });
}

#[test]
fn publish_flushes_inbound_ack_and_publication_together() {
    let mut io = MockConnection::default();
    let mut inspect = io.clone();
    io.push_rx(&connack());
    let mut session = session();
    let mut conn = block_on(session.connect(io)).unwrap();
    inspect.push_rx(&inbound_publish_qos1(9, "set", b"x"));
    assert_eq!(block_on(conn.recv()).unwrap().payload(), b"x");
    let op = publish_qos1(&mut conn, "data", b"y").unwrap().unwrap();
    let expected = [
        outbound_puback(9).as_slice(),
        &inbound_publish_qos1(1, "data", b"y"),
    ]
    .concat();
    assert_eq!(inspect.inner.borrow().flush_groups[1..], [expected]);
    assert!(conn.is_pending(&op));
    inspect.push_rx(&puback(1));
    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(conn.is_complete(&op));
}

#[test]
fn resumed_replay_flushes_eight_publications_once() {
    let mut first = MockConnection::default();
    let inspect_first = first.clone();
    first.push_rx(&connack());
    let mut session = session();
    let mut conn = block_on(session.connect(first)).unwrap();
    for value in 0..8 {
        publish_qos1_ok(&mut conn, "data", &[value]);
    }
    let mut expected = inspect_first.tx()[1..].to_vec();
    for packet in &mut expected {
        packet[0] |= 0x08;
    }
    drop(conn);
    let mut second = MockConnection::default();
    let inspect_second = second.clone();
    second.push_rx(&connack_session_present());
    let mut conn = block_on(session.connect(second)).unwrap();
    assert!(block_on(conn.drive()).unwrap().is_none());
    assert_eq!(
        inspect_second.inner.borrow().flush_groups[1..],
        [expected.concat()]
    );
}

#[test]
fn cancelled_flush_finishes_before_new_publication_writes() {
    let mut io = MockConnection::default();
    let mut inspect = io.clone();
    io.push_rx(&connack());
    let mut session = session();
    let mut conn = block_on(session.connect(io)).unwrap();
    inspect.inner.borrow_mut().write_limit = Some(3);
    inspect.pend_flushes(2);
    assert!(
        poll_once(
            Box::pin(conn.publish(Publication::bytes("first", b"one").qos(QoS::AtLeastOnce)))
                .as_mut()
        )
        .is_pending()
    );
    let first = inspect.tx();
    assert!(
        poll_once(
            Box::pin(conn.publish(Publication::bytes("second", b"two").qos(QoS::AtLeastOnce)))
                .as_mut()
        )
        .is_pending()
    );
    assert_eq!(inspect.tx(), first);
    assert!(block_on(conn.drive()).unwrap().is_none());
    let expected = [
        inbound_publish_qos1(1, "first", b"one"),
        inbound_publish_qos1(2, "second", b"two"),
    ];
    assert_eq!(inspect.inner.borrow().flush_groups[1..], expected);
    assert_eq!(inspect.tx()[1..].concat(), expected.concat());
}

#[test]
fn disconnect_finishes_a_cancelled_publication_before_terminal_packet() {
    let mut io = MockConnection::default();
    let mut inspect = io.clone();
    io.push_rx(&connack());
    let mut session = session();
    let mut conn = block_on(session.connect(io)).unwrap();
    inspect.pend_flushes(1);
    assert!(
        poll_once(
            Box::pin(conn.publish(Publication::bytes("data", b"x").qos(QoS::AtLeastOnce),))
                .as_mut()
        )
        .is_pending()
    );
    block_on(conn.disconnect()).unwrap();
    let expected = [
        inbound_publish_qos1(1, "data", b"x"),
        disconnect_req().to_vec(),
    ];
    assert_eq!(inspect.tx()[1..].concat(), expected.concat());
    assert_eq!(inspect.inner.borrow().flush_groups[1..], expected);
}

#[test]
fn flush_failure_disconnects_and_replays_retained_publication() {
    let mut first = MockConnection::default();
    let inspect_first = first.clone();
    first.push_rx(&connack());
    let mut session = session();
    let mut conn = block_on(session.connect(first)).unwrap();
    inspect_first.inner.borrow_mut().flush_error = Some(ErrorKind::ConnectionReset);
    assert!(matches!(
        publish_qos1(&mut conn, "data", b"x"),
        Err(PubError::Session(Error::Transport(
            ErrorKind::ConnectionReset
        )))
    ));
    assert!(!conn.is_connected());
    drop(conn);
    let mut second = MockConnection::default();
    let mut inspect_second = second.clone();
    second.push_rx(&connack_session_present());
    let mut conn = block_on(session.connect(second)).unwrap();
    assert!(block_on(conn.drive()).unwrap().is_none());
    let mut expected = inbound_publish_qos1(1, "data", b"x");
    expected[0] |= 0x08;
    assert_eq!(inspect_second.inner.borrow().flush_groups[1..], [expected]);
    inspect_second.push_rx(&puback(1));
    assert!(poll_now(&mut conn).unwrap().is_none());
    assert!(conn.session().is_publish_quiescent());
}

#[test]
fn streaming_buffer_can_fill_partway_through_a_burst() {
    let mut io = MockConnection::default();
    let mut inspect = io.clone();
    io.push_rx(&connack());
    let mut session = session();
    let mut conn = block_on(session.connect(io)).unwrap();
    inspect.inner.borrow_mut().buffer_capacity = Some(12);
    inspect.push_rx(&inbound_publish_qos1(9, "set", b"x"));
    assert_eq!(block_on(conn.recv()).unwrap().payload(), b"x");
    publish_qos1_ok(&mut conn, "data", b"y");
    let expected = [
        outbound_puback(9).as_slice(),
        &inbound_publish_qos1(1, "data", b"y"),
    ]
    .concat();
    assert_eq!(inspect.tx()[1..].concat(), expected);
    let inner = inspect.inner.borrow();
    assert_eq!(inner.automatic_flushes, 1);
    assert_eq!(inner.flush_groups.len(), 2); // CONNECT, then the whole MQTT burst.
    assert!(inner.buffered.is_empty());
}

#[test]
fn qos2_quota_waits_for_pubcomp() {
    let mut session = session();
    let mut io = MockConnection::default();
    io.push_rx(&connack_receive_max(1));
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    publish_qos2_ok(&mut conn, "a", b"x");
    io.push_rx(&pubrec(1));
    wait_for_tx_frame(&mut conn, &io, &outbound_pubrel(1));
    assert!(
        !conn.can_publish(QoS::ExactlyOnce),
        "PUBREC incorrectly replenished the quota"
    );
    io.push_rx(&pubcomp(1));
    for _ in 0..WAIT_STEPS {
        block_on(conn.poll()).unwrap();
        if conn.can_publish(QoS::ExactlyOnce) {
            return;
        }
    }
    panic!("PUBCOMP did not replenish quota");
}

#[test]
fn qos2_release_capacity_applies_backpressure_until_pubcomp() {
    for protocol in [V3, minimq::Protocol::V5] {
        let mut session = Session::new(config().protocol(protocol));
        let mut io = MockConnection::default();
        if protocol == V3 {
            io.push_rx(&[0x20, 2, 0, 0]);
        } else {
            io.push_rx(&connack());
        }
        let mut conn = block_on(session.connect(io.clone())).unwrap();
        let mut operations = Vec::new();
        for id in 1..=u8::MAX {
            if !conn.can_publish(QoS::ExactlyOnce) {
                break;
            }
            let op = publish_qos2(&mut conn, "a", b"x").unwrap().unwrap();
            operations.push(op);
            io.push_rx(&pubrec(id.into()));
            let release = if protocol == V3 {
                vec![0x62, 2, 0, id]
            } else {
                outbound_pubrel(id.into()).to_vec()
            };
            wait_for_tx_frame(&mut conn, &io, &release);
            assert!(operations.iter().all(|op| conn.is_pending(op)));
        }
        assert!(!operations.is_empty());
        assert!(!conn.can_publish(QoS::ExactlyOnce));
        assert!(publish_qos2(&mut conn, "a", b"x").is_err());
        io.push_rx(&pubcomp(1));
        for _ in 0..WAIT_STEPS {
            block_on(conn.poll()).unwrap();
            if conn.is_complete(&operations[0]) {
                break;
            }
        }
        assert!(conn.is_complete(&operations[0]));
        assert!(operations[1..].iter().all(|op| conn.is_pending(op)));
        assert!(conn.can_publish(QoS::ExactlyOnce));
        publish_qos2_ok(&mut conn, "a", b"x");
    }
}

#[test]
fn server_keepalive_does_not_change_reconnect_config() {
    let mut session = Session::new(config());
    let mut io = MockConnection::default();
    io.push_rx(&[0x20, 6, 0, 0, 3, 0x13, 0, 1]);
    drop(block_on(session.connect(io)).unwrap());
    let mut io = MockConnection::default();
    io.push_rx(&connack_session_present());
    drop(block_on(session.connect(io.clone())).unwrap());
    assert_eq!(
        &io.tx().concat()[10..12],
        &[0, 60],
        "previous broker override leaked into CONNECT"
    );
}

#[test]
fn replay_consumes_new_connection_quota() {
    let mut session = session();
    let mut io = MockConnection::default();
    io.push_rx(&connack());
    let mut conn = block_on(session.connect(io)).unwrap();
    publish_qos1_ok(&mut conn, "a", b"x");
    drop(conn);
    let mut io = MockConnection::default();
    let mut ack = connack_receive_max(1);
    ack[2] = 1;
    io.push_rx(&ack);
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    block_on(conn.drive()).unwrap();
    assert!(
        !conn.can_publish(QoS::AtLeastOnce),
        "replayed PUBLISH did not consume the new quota"
    );
}

#[test]
fn replay_waits_for_smaller_quota_and_still_services_inbound_acks() {
    let mut session = session();
    let mut io = MockConnection::default();
    io.push_rx(&connack());
    let mut conn = block_on(session.connect(io)).unwrap();
    for _ in 0..3 {
        publish_qos1_ok(&mut conn, "a", b"x");
    }
    drop(conn);
    let mut io = MockConnection::default();
    let mut ack = connack_receive_max(1);
    ack[2] = 1;
    io.push_rx(&ack);
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    io.inner.borrow_mut().tx.clear();
    for id in 1..=3 {
        // Independent wire expectation: DUP QoS 1, topic a, empty properties, payload x.
        let expected = [0x3a, 7, 0, 1, b'a', 0, id, 0, b'x'];
        wait_for_tx_frame(&mut conn, &io, &expected);
        for _ in 0..4 {
            poll_now(&mut conn).unwrap();
        }
        assert_eq!(io.tx().concat(), expected);
        assert!(!conn.can_publish(QoS::AtLeastOnce));
        io.inner.borrow_mut().tx.clear();
        // Quota must only block outgoing PUBLISH, not the inbound acknowledgement.
        io.push_rx(&[0x32, 7, 0, 1, b'b', 0, 99, 0, b'y']);
        assert_eq!(block_on(conn.recv()).unwrap().payload(), b"y");
        wait_for_tx_frame(&mut conn, &io, &outbound_puback(99));
        io.inner.borrow_mut().tx.clear();
        io.push_rx(&puback(id.into()));
    }
    for _ in 0..WAIT_STEPS {
        block_on(conn.poll()).unwrap();
        if conn.can_publish(QoS::AtLeastOnce) {
            return;
        }
    }
    panic!("last PUBACK did not restore quota");
}

#[test]
fn negative_pubrec_restores_quota_but_stale_acks_do_not() {
    let mut session = session();
    let mut io = MockConnection::default();
    io.push_rx(&connack_receive_max(1));
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    let op = publish_qos2(&mut conn, "a", b"x").unwrap().unwrap();
    io.push_rx(&pubrec_reason(1, 0x97));
    expect_poll_error(&mut conn, |err| {
        matches!(
            err,
            Error::Peer(PeerError::Rejected(ReasonCode::QuotaExceeded))
        )
    });
    assert!(conn.is_complete(&op));
    assert!(conn.can_publish(QoS::ExactlyOnce));
    publish_qos2_ok(&mut conn, "a", b"x");
    io.push_rx(&pubrec_reason(1, 0x97));
    io.push_rx(&pubcomp(1));
    for _ in 0..WAIT_STEPS {
        poll_now(&mut conn).unwrap();
    }
    assert!(!conn.can_publish(QoS::ExactlyOnce));
}

#[test]
fn pubrel_replay_preserves_original_send_order() {
    for protocol in [V3, minimq::Protocol::V5] {
        let mut session = Session::new(config().protocol(protocol));
        let mut io = MockConnection::default();
        if protocol == V3 {
            io.push_rx(&[0x20, 2, 0, 0]);
        } else {
            io.push_rx(&connack());
        }
        let mut conn = block_on(session.connect(io.clone())).unwrap();
        let first = publish_qos2(&mut conn, "a", b"x").unwrap().unwrap();
        let second = publish_qos2(&mut conn, "a", b"y").unwrap().unwrap();
        let release = |id: u8| [0x62, 2, 0, id];
        for id in [2, 1] {
            io.push_rx(&pubrec(id.into()));
            wait_for_tx_frame(&mut conn, &io, &release(id));
        }
        drop(conn);
        let mut io = MockConnection::default();
        if protocol == V3 {
            io.push_rx(&[0x20, 2, 1, 0]);
        } else {
            io.push_rx(&connack_session_present());
        }
        let mut conn = block_on(session.connect(io.clone())).unwrap();
        io.inner.borrow_mut().tx.clear();
        block_on(conn.drive()).unwrap();
        assert_eq!(io.tx().concat(), [release(2), release(1)].concat());
        assert!(conn.is_pending(&first) && conn.is_pending(&second));
        io.push_rx(&pubcomp(2));
        for _ in 0..WAIT_STEPS {
            poll_now(&mut conn).unwrap();
        }
        assert!(conn.is_pending(&first) && conn.is_complete(&second));
    }
}

#[test]
fn inbound_qos2_ack_failure_does_not_suppress_delivery_after_resume() {
    let mut session = Session::new(config().session_expiry_interval(60));
    let mut io = MockConnection::default();
    io.push_rx(&connack_max_packet_size(3));
    io.push_rx(&inbound_publish_qos2(9, "a", b"x"));
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    expect_poll_error(&mut conn, |err| {
        matches!(err, Error::Resource(ResourceError::PacketTooLarge))
    });
    assert!(!conn.is_connected());
    drop(conn);
    let mut io = MockConnection::default();
    io.push_rx(&connack_session_present());
    let mut packet = inbound_publish_qos2(9, "a", b"x");
    packet[0] |= 8;
    io.push_rx(&packet);
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    for _ in 0..WAIT_STEPS {
        if let Some(message) = poll_now(&mut conn).unwrap() {
            assert_eq!(message.payload(), b"x");
            return;
        }
    }
    panic!(
        "message never delivered; retransmission suppressed: {:?}",
        io.tx()
    );
}

#[test]
fn v5_receive_max_overflow_closes_without_acknowledging() {
    let mut session = session();
    let mut io = MockConnection::default();
    io.push_rx(&connack());
    let mut conn = block_on(session.connect(io.clone())).unwrap();
    // This CONNECT fixture has one Receive Maximum property (identifier 0x21).
    let connect = io.tx().concat();
    let property = connect.windows(3).find(|bytes| bytes[0] == 0x21).unwrap();
    let receive_max = u16::from_be_bytes([property[1], property[2]]);
    for id in 1..=receive_max {
        io.push_rx(&inbound_publish_qos2(id, "a", b"x"));
        assert_eq!(block_on(conn.recv()).unwrap().payload(), b"x");
        wait_for_tx_frame(&mut conn, &io, &outbound_pubrec(id));
    }
    // A retransmission still needs its acknowledgement while capacity is full.
    io.inner.borrow_mut().tx.clear();
    let mut duplicate = inbound_publish_qos2(1, "a", b"x");
    duplicate[0] |= 8;
    io.push_rx(&duplicate);
    wait_for_tx_frame(&mut conn, &io, &outbound_pubrec(1));
    assert!(conn.is_connected());
    let excess = receive_max + 1;
    io.inner.borrow_mut().tx.clear();
    io.push_rx(&inbound_publish_qos2(excess, "a", b"x"));
    for _ in 0..WAIT_STEPS {
        if poll_now(&mut conn).is_err() {
            break;
        }
    }
    assert!(
        io.tx().is_empty(),
        "overflow must not acknowledge the publication"
    );
    assert!(
        !conn.is_connected(),
        "peer exceeded advertised receive maximum"
    );
}
