mod support;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use minimq::{
    Broker, Buffers, ConfigBuilder, Error, MqttClient, ProtocolError, PubError, Publication, QoS,
    Runner,
    timer::Timer,
    transport::Connector,
};
use std::{
    cell::RefCell,
    collections::VecDeque,
};
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
            return Ok(0);
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
    type ConnectError = ErrorKind;
    type IoError = ErrorKind;
    type Connection<'a> = MockConnection;

    async fn connect<'a, const N: usize>(
        &'a self,
        _broker: &Broker<N>,
    ) -> Result<Self::Connection<'a>, Self::ConnectError> {
        self.connections
            .borrow_mut()
            .pop_front()
            .ok_or(ErrorKind::ConnectionReset)
    }
}

#[derive(Default)]
struct MockTimer {
    now: u64,
}

impl Timer for MockTimer {
    type Error = ();

    fn now(&mut self) -> Result<u64, Self::Error> {
        Ok(self.now)
    }

    async fn sleep_until(&mut self, deadline_ms: u64) -> Result<(), Self::Error> {
        self.now = self.now.max(deadline_ms);
        Ok(())
    }
}

fn client() -> MqttClient<'static> {
    let rx = Box::leak(Box::new([0; 128]));
    let tx = Box::leak(Box::new([0; 128]));
    let inflight = Box::leak(Box::new([0; 1024]));
    let broker = Broker::from("127.0.0.1".parse::<std::net::IpAddr>().unwrap());
    MqttClient::new(
        ConfigBuilder::new(broker, Buffers { rx, tx, inflight })
            .client_id("test")
            .unwrap()
            .build(),
    )
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

fn disconnect() -> [u8; 2] {
    [0xE0, 0x00]
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

fn establish(client: &mut MqttClient<'static>, connection: &mut MockConnection) {
    block_on(client.connect(connection, 0)).unwrap();
    connection.push_rx(&connack());
    assert!(block_on(client.read(connection, 0)).unwrap().is_none());
    assert!(client.is_connected());
}

#[test]
fn disconnected_publish_is_error() {
    let mut client = client();
    let mut connection = MockConnection::default();
    let result = block_on(client.publish(
        &mut connection,
        Publication::new("data", b"payload").qos(QoS::AtLeastOnce),
    ));
    assert!(matches!(
        result,
        Err(PubError::Error(Error::Minimq(
            minimq::MinimqError::Protocol(ProtocolError::NotConnected)
        )))
    ));
}

#[test]
fn inflight_packets_are_not_replayed_during_normal_maintain() {
    let mut client = client();
    let mut connection = MockConnection::default();
    establish(&mut client, &mut connection);

    let tx_before = connection.tx.len();
    block_on(client.publish(
        &mut connection,
        Publication::new("data", b"x").qos(QoS::AtLeastOnce),
    ))
    .unwrap();
    let tx_after_publish = connection.tx.len();
    assert_eq!(tx_after_publish, tx_before + 1);

    block_on(client.maintain(&mut connection, 1)).unwrap();
    block_on(client.maintain(&mut connection, 2)).unwrap();
    assert_eq!(connection.tx.len(), tx_after_publish);
}

#[test]
fn broker_disconnect_marks_inflight_for_replay_on_resume() {
    let mut client = client();
    let mut first = MockConnection::default();
    establish(&mut client, &mut first);

    block_on(client.publish(
        &mut first,
        Publication::new("data", b"x").qos(QoS::AtLeastOnce),
    ))
    .unwrap();
    let sent = first.tx.last().unwrap().clone();

    first.push_rx(&disconnect());
    assert!(block_on(client.read(&mut first, 1)).unwrap().is_none());
    assert!(!client.is_connected());

    let mut second = MockConnection::default();
    block_on(client.connect(&mut second, 2)).unwrap();
    second.push_rx(&connack_session_present());
    assert!(block_on(client.read(&mut second, 2)).unwrap().is_none());
    block_on(client.maintain(&mut second, 3)).unwrap();
    let mut replay = sent;
    replay[0] |= 1 << 3;
    assert!(second.tx.iter().any(|packet| packet == &replay));
}

#[test]
fn inflight_metadata_exhaustion_is_reported() {
    let mut client = client();
    let mut connection = MockConnection::default();
    establish(&mut client, &mut connection);

    for index in 0..16 {
        block_on(client.publish(
            &mut connection,
            Publication::new("data", b"x").qos(QoS::AtLeastOnce),
        ))
        .unwrap_or_else(|_| panic!("publish {index} failed"));
    }

    let result = block_on(client.publish(
        &mut connection,
        Publication::new("data", b"x").qos(QoS::AtLeastOnce),
    ));
    assert!(matches!(
        result,
        Err(PubError::Error(Error::Minimq(
            minimq::MinimqError::Protocol(ProtocolError::InflightMetadataExhausted)
        )))
    ));
}

#[test]
fn outbound_qos_acks_can_arrive_out_of_order() {
    let mut client = client();
    let mut connection = MockConnection::default();
    establish(&mut client, &mut connection);

    for _ in 0..3 {
        block_on(client.publish(
            &mut connection,
            Publication::new("data", b"x").qos(QoS::AtLeastOnce),
        ))
        .unwrap();
    }
    assert!(client.pending_messages());

    connection.push_rx(&puback(2));
    connection.push_rx(&puback(1));
    connection.push_rx(&puback(3));
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());
    assert!(!client.pending_messages());
}

#[test]
fn outbound_qos2_flow_allows_out_of_order_pubrec_and_pubcomp() {
    let mut client = client();
    let mut connection = MockConnection::default();
    establish(&mut client, &mut connection);

    for _ in 0..2 {
        block_on(client.publish(
            &mut connection,
            Publication::new("data", b"x").qos(QoS::ExactlyOnce),
        ))
        .unwrap();
    }

    connection.push_rx(&pubrec(2));
    connection.push_rx(&pubrec(1));
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());

    connection.push_rx(&pubcomp(2));
    connection.push_rx(&pubcomp(1));
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());
    assert!(!client.pending_messages());
}

#[test]
fn inbound_qos2_marks_pending_until_pubrel() {
    let mut client = client();
    let mut connection = MockConnection::default();
    establish(&mut client, &mut connection);

    connection.push_rx(&inbound_publish_qos2(9, "data", b"x"));
    let publish = block_on(client.read(&mut connection, 1)).unwrap().unwrap();
    assert_eq!(publish.topic, "data");
    assert!(client.pending_messages());

    connection.push_rx(&pubrel(9));
    assert!(block_on(client.read(&mut connection, 1)).unwrap().is_none());
    assert!(!client.pending_messages());
}

#[test]
fn connack_receive_maximum_clamps_local_quota() {
    let mut client = client();
    let mut connection = MockConnection::default();
    block_on(client.connect(&mut connection, 0)).unwrap();
    connection.push_rx(&connack_receive_max(2));
    assert!(block_on(client.read(&mut connection, 0)).unwrap().is_none());

    block_on(client.publish(
        &mut connection,
        Publication::new("data", b"x").qos(QoS::AtLeastOnce),
    ))
    .unwrap();
    block_on(client.publish(
        &mut connection,
        Publication::new("data", b"x").qos(QoS::AtLeastOnce),
    ))
    .unwrap();

    let result = block_on(client.publish(
        &mut connection,
        Publication::new("data", b"x").qos(QoS::AtLeastOnce),
    ));
    assert!(matches!(result, Err(PubError::Error(Error::NotReady))));
}

#[test]
fn runner_allows_publish_after_message_borrow_is_dropped() {
    let mut connection = MockConnection::default();
    connection.push_rx(&connack());
    connection.push_rx(&inbound_publish_qos1(7, "cmd", b"payload"));

    let connector = MockConnector::new(connection);
    let mut timer = MockTimer::default();
    let mut client = client();
    let mut runner = Runner::new(&mut client, &connector, &mut timer);

    {
        let message = loop {
            if let Some(message) = block_on(runner.poll()).unwrap() {
                break message;
            }
        };
        assert_eq!(message.topic, "cmd");
        assert_eq!(message.payload, b"payload");
    }

    block_on(runner.publish(Publication::new("reply", b"ok").qos(QoS::AtLeastOnce))).unwrap();
}

#[test]
fn runner_reconnects_after_write_error() {
    let mut first = MockConnection::default();
    first.push_rx(&connack());
    first.fail_write_after(1, ErrorKind::ConnectionReset);

    let mut second = MockConnection::default();
    second.push_rx(&connack_session_present());

    let connector = MockConnector::with_connections([first, second]);
    let mut timer = MockTimer::default();
    let mut client = client();
    let mut runner = Runner::new(&mut client, &connector, &mut timer);

    let error = block_on(runner.publish(Publication::new("reply", b"ok").qos(QoS::AtLeastOnce)))
        .unwrap_err();
    assert!(matches!(
        error,
        minimq::RunnerPubError::Publish(PubError::Error(Error::Network(ErrorKind::ConnectionReset)))
    ));

    block_on(runner.publish(Publication::new("reply", b"ok").qos(QoS::AtLeastOnce))).unwrap();
}
