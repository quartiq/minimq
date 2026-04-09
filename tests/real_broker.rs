mod support;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use minimq::{
    Broker, Buffers, ConfigBuilder, Error, MqttClient, Publication, QoS,
    transport::{Connector, DnsTcpConnector, TcpConnector},
    types::TopicFilter,
};
use support::block_on;
use std::{
    io,
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const BROKER_ADDR_ENV: &str = "MINIMQ_REAL_BROKER_ADDR";
const BROKER_HOST_ENV: &str = "MINIMQ_REAL_BROKER_HOST";
const IO_TIMEOUT: Duration = Duration::from_millis(200);
const STEP_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct StdConnection(TcpStream);

impl ErrorType for StdConnection {
    type Error = ErrorKind;
}

fn io_kind(kind: io::ErrorKind) -> ErrorKind {
    match kind {
        io::ErrorKind::NotFound => ErrorKind::NotFound,
        io::ErrorKind::PermissionDenied => ErrorKind::PermissionDenied,
        io::ErrorKind::ConnectionRefused => ErrorKind::ConnectionRefused,
        io::ErrorKind::ConnectionReset => ErrorKind::ConnectionReset,
        io::ErrorKind::ConnectionAborted => ErrorKind::ConnectionAborted,
        io::ErrorKind::NotConnected => ErrorKind::NotConnected,
        io::ErrorKind::AddrInUse => ErrorKind::AddrInUse,
        io::ErrorKind::AddrNotAvailable => ErrorKind::AddrNotAvailable,
        io::ErrorKind::BrokenPipe => ErrorKind::BrokenPipe,
        io::ErrorKind::AlreadyExists => ErrorKind::AlreadyExists,
        io::ErrorKind::InvalidInput => ErrorKind::InvalidInput,
        io::ErrorKind::InvalidData => ErrorKind::InvalidData,
        io::ErrorKind::TimedOut => ErrorKind::TimedOut,
        io::ErrorKind::Interrupted => ErrorKind::Interrupted,
        io::ErrorKind::Unsupported => ErrorKind::Unsupported,
        io::ErrorKind::OutOfMemory => ErrorKind::OutOfMemory,
        io::ErrorKind::WriteZero => ErrorKind::WriteZero,
        _ => ErrorKind::Other,
    }
}

impl Read for StdConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        io::Read::read(&mut self.0, buf).map_err(|err| io_kind(err.kind()))
    }
}

impl Write for StdConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        io::Write::write(&mut self.0, buf).map_err(|err| io_kind(err.kind()))
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        io::Write::flush(&mut self.0).map_err(|err| io_kind(err.kind()))
    }
}

#[derive(Debug, Copy, Clone)]
struct StdStack {
    io_timeout: Duration,
}

impl StdStack {
    fn new(io_timeout: Duration) -> Self {
        Self { io_timeout }
    }

    fn connect_stream(&self, remote: SocketAddr) -> Result<StdConnection, io::Error> {
        let stream = TcpStream::connect_timeout(&remote, STEP_TIMEOUT)?;
        stream.set_read_timeout(Some(self.io_timeout))?;
        stream.set_write_timeout(Some(STEP_TIMEOUT))?;
        stream.set_nodelay(true)?;
        Ok(StdConnection(stream))
    }
}

impl TcpConnect for StdStack {
    type Error = ErrorKind;
    type Connection<'a> = StdConnection;

    async fn connect<'a>(&'a self, remote: SocketAddr) -> Result<Self::Connection<'a>, Self::Error> {
        self.connect_stream(remote).map_err(|err| io_kind(err.kind()))
    }
}

#[derive(Debug, Copy, Clone, Default)]
struct StdDns;

impl Dns for StdDns {
    type Error = ErrorKind;

    async fn get_host_by_name(&self, host: &str, _addr_type: AddrType) -> Result<IpAddr, Self::Error> {
        (host, 0)
            .to_socket_addrs()
            .map_err(|err| io_kind(err.kind()))?
            .next()
            .map(|addr| addr.ip())
            .ok_or(ErrorKind::NotFound)
    }

    async fn get_host_by_address(&self, _addr: IpAddr, _result: &mut [u8]) -> Result<usize, Self::Error> {
        Err(ErrorKind::Unsupported)
    }
}

fn socket_broker() -> Option<SocketAddr> {
    let raw = match std::env::var(BROKER_ADDR_ENV) {
        Ok(raw) => raw,
        Err(_) => return None,
    };
    Some(
        raw.parse()
            .unwrap_or_else(|_| panic!("invalid {BROKER_ADDR_ENV} value: {raw}")),
    )
}

fn hostname_broker() -> Option<String> {
    std::env::var(BROKER_HOST_ENV).ok()
}

fn now_ms(start: Instant) -> u64 {
    start.elapsed().as_millis() as u64
}

fn timed_out(err: ErrorKind) -> bool {
    err == ErrorKind::TimedOut
}

fn unique_client_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq-{label}-{nanos}")
}

fn unique_topic() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq/test/{nanos}")
}

fn client(broker: Broker, client_id: &str) -> MqttClient<'static> {
    let rx = Box::leak(Box::new([0; 1024]));
    let tx = Box::leak(Box::new([0; 1024]));
    let inflight = Box::leak(Box::new([0; 1024]));
    MqttClient::new(
        ConfigBuilder::new(broker, Buffers { rx, tx, inflight })
            .client_id(client_id)
            .unwrap()
            .build(),
    )
}

fn connect_client<C>(client: &mut MqttClient<'static>, connection: &mut C, start: Instant)
where
    C: Read + Write + ErrorType<Error = ErrorKind>,
{
    block_on(client.connect(connection, now_ms(start))).unwrap();
    let deadline = Instant::now() + STEP_TIMEOUT;
    while !client.is_connected() {
        match block_on(client.read(connection, now_ms(start))) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("unexpected publish during connect"),
            Err(Error::Network(err)) if timed_out(err) => {
                assert!(Instant::now() < deadline, "timed out waiting for CONNACK");
            }
            Err(err) => panic!("connect failed: {err:?}"),
        }
    }
}

fn subscribe_topic<C>(
    client: &mut MqttClient<'static>,
    connection: &mut C,
    topic: &str,
    start: Instant,
) where
    C: Read + Write + ErrorType<Error = ErrorKind>,
{
    let topics = [TopicFilter::new(topic)];
    block_on(client.subscribe(connection, &topics, &[])).unwrap();

    let deadline = Instant::now() + STEP_TIMEOUT;
    while client.subscriptions_pending() {
        match block_on(client.read(connection, now_ms(start))) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("unexpected publish while awaiting SUBACK"),
            Err(Error::Network(err)) if timed_out(err) => {
                assert!(Instant::now() < deadline, "timed out waiting for SUBACK");
            }
            Err(err) => panic!("subscribe failed: {err:?}"),
        }
    }
}

fn publish_qos1<C>(
    client: &mut MqttClient<'static>,
    connection: &mut C,
    topic: &str,
    payload: &[u8],
    start: Instant,
) where
    C: Read + Write + ErrorType<Error = ErrorKind>,
{
    block_on(client.publish(
        connection,
        Publication::new(topic, payload).qos(QoS::AtLeastOnce),
    ))
    .unwrap();

    let deadline = Instant::now() + STEP_TIMEOUT;
    while client.pending_messages() {
        match block_on(client.read(connection, now_ms(start))) {
            Ok(None) => {}
            Ok(Some(_)) => panic!("publisher unexpectedly received a publish"),
            Err(Error::Network(err)) if timed_out(err) => {
                assert!(Instant::now() < deadline, "timed out waiting for PUBACK");
            }
            Err(err) => panic!("publish did not complete: {err:?}"),
        }
    }
}

fn await_publish<C>(
    client: &mut MqttClient<'static>,
    connection: &mut C,
    start: Instant,
) -> (String, Vec<u8>, QoS)
where
    C: Read + Write + ErrorType<Error = ErrorKind>,
{
    let deadline = Instant::now() + STEP_TIMEOUT;
    loop {
        match block_on(client.read(connection, now_ms(start))) {
            Ok(Some(message)) => {
                return (
                    message.topic.to_string(),
                    message.payload.to_vec(),
                    message.qos,
                );
            }
            Ok(None) => {}
            Err(Error::Network(err)) if timed_out(err) => {
                assert!(Instant::now() < deadline, "timed out waiting for publish");
            }
            Err(err) => panic!("read failed: {err:?}"),
        }
    }
}

fn assert_roundtrip<C>(
    subscriber: &mut MqttClient<'static>,
    publisher: &mut MqttClient<'static>,
    topic: &str,
    payload: &[u8],
    start: Instant,
    subscriber_connector: C,
    publisher_connector: C,
) where
    C: Connector<IoError = ErrorKind>,
    C::ConnectError: core::fmt::Debug,
{
    let mut subscriber_connection = block_on(subscriber_connector.connect(subscriber.broker())).unwrap();
    let mut publisher_connection = block_on(publisher_connector.connect(publisher.broker())).unwrap();

    connect_client(subscriber, &mut subscriber_connection, start);
    subscribe_topic(subscriber, &mut subscriber_connection, topic, start);
    connect_client(publisher, &mut publisher_connection, start);
    publish_qos1(publisher, &mut publisher_connection, topic, payload, start);

    let (received_topic, received_payload, received_qos) =
        await_publish(subscriber, &mut subscriber_connection, start);
    assert_eq!(received_topic, topic);
    assert_eq!(received_payload, payload);
    assert_eq!(received_qos, QoS::AtLeastOnce);
}

#[test]
fn real_broker_qos1_roundtrip_over_tcp_connector() {
    let Some(addr) = socket_broker() else {
        eprintln!("skipping real broker test; set {BROKER_ADDR_ENV}=host:port");
        return;
    };

    let mut subscriber = client(Broker::socket_addr(addr), &unique_client_id("sub"));
    let mut publisher = client(Broker::socket_addr(addr), &unique_client_id("pub"));
    let topic = unique_topic();
    let payload = b"hello from minimq";
    let start = Instant::now();

    assert_roundtrip(
        &mut subscriber,
        &mut publisher,
        &topic,
        payload,
        start,
        TcpConnector::new(StdStack::new(IO_TIMEOUT)),
        TcpConnector::new(StdStack::new(IO_TIMEOUT)),
    );
}

#[test]
fn real_broker_qos1_roundtrip_over_dns_connector() {
    let Some(host) = hostname_broker() else {
        eprintln!("skipping hostname broker test; set {BROKER_HOST_ENV}=hostname");
        return;
    };

    let mut broker = Broker::host(&host).unwrap();
    let port = socket_broker().map(|addr| addr.port()).unwrap_or(1883);
    broker.set_port(port);

    let mut subscriber = client(broker.clone(), &unique_client_id("dns-sub"));
    let mut publisher = client(broker, &unique_client_id("dns-pub"));
    let topic = unique_topic();
    let payload = b"hello over dns";
    let start = Instant::now();

    let stack = StdStack::new(IO_TIMEOUT);
    let dns = StdDns;
    assert_roundtrip(
        &mut subscriber,
        &mut publisher,
        &topic,
        payload,
        start,
        DnsTcpConnector::new(stack, dns),
        DnsTcpConnector::new(stack, dns),
    );
}
