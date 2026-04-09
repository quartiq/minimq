mod support;

use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_nal_async::{AddrType, Dns, TcpConnect};
use minimq::{
    Broker, Buffers, ConfigBuilder, Error, Event, Publication, QoS, Session,
    transport::{DnsTcpConnector, TcpConnector},
    types::TopicFilter,
};
use std::{
    io,
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use support::block_on;

const BROKER_ADDR_ENV: &str = "MINIMQ_REAL_BROKER_ADDR";
const BROKER_HOST_ENV: &str = "MINIMQ_REAL_BROKER_HOST";
const IO_TIMEOUT: Duration = Duration::from_millis(200);

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
}

impl TcpConnect for StdStack {
    type Error = ErrorKind;
    type Connection<'a> = StdConnection;

    async fn connect<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let stream = TcpStream::connect_timeout(&remote, Duration::from_secs(5))
            .map_err(|err| io_kind(err.kind()))?;
        stream
            .set_read_timeout(Some(self.io_timeout))
            .map_err(|err| io_kind(err.kind()))?;
        stream
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|err| io_kind(err.kind()))?;
        stream
            .set_nodelay(true)
            .map_err(|err| io_kind(err.kind()))?;
        Ok(StdConnection(stream))
    }
}

#[derive(Debug, Copy, Clone, Default)]
struct StdDns;

impl Dns for StdDns {
    type Error = io::ErrorKind;

    async fn get_host_by_name(
        &self,
        host: &str,
        _addr_type: AddrType,
    ) -> Result<IpAddr, Self::Error> {
        (host, 0)
            .to_socket_addrs()
            .map_err(|err| err.kind())?
            .next()
            .map(|addr| addr.ip())
            .ok_or(io::ErrorKind::NotFound)
    }

    async fn get_host_by_address(
        &self,
        _addr: IpAddr,
        _result: &mut [u8],
    ) -> Result<usize, Self::Error> {
        Err(io::ErrorKind::Unsupported)
    }
}

fn socket_broker() -> Option<SocketAddr> {
    let raw = std::env::var(BROKER_ADDR_ENV).ok()?;
    Some(
        raw.parse()
            .unwrap_or_else(|_| panic!("invalid {BROKER_ADDR_ENV} value: {raw}")),
    )
}

fn hostname_broker() -> Option<String> {
    std::env::var(BROKER_HOST_ENV).ok()
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

fn config<'a>(broker: Broker<'a>, client_id: &str) -> minimq::Config<'a> {
    let rx = Box::leak(Box::new([0; 1024]));
    let tx = Box::leak(Box::new([0; 1024]));
    let inflight = Box::leak(Box::new([0; 1024]));
    ConfigBuilder::new(broker, Buffers { rx, tx, inflight })
        .client_id(client_id)
        .unwrap()
        .build()
}

fn poll_until_ready<'a, C>(
    session: &mut Session<'_, 'a, C>,
    want_inbound: bool,
) -> Option<(String, Vec<u8>, QoS)>
where
    C: minimq::transport::Connector,
{
    for _ in 0..200 {
        match block_on(session.poll()) {
            Ok(Event::Idle | Event::Connected | Event::Reconnected) if !want_inbound => {
                return None;
            }
            Ok(Event::Inbound(message)) if want_inbound => {
                return Some((
                    message.topic.to_string(),
                    message.payload.to_vec(),
                    message.qos,
                ));
            }
            Ok(_) => {}
            Err(Error::Transport(ErrorKind::TimedOut | ErrorKind::Interrupted)) => {}
            Err(err) => panic!("session poll failed: {err:?}"),
        }
    }
    panic!("timed out waiting for broker activity");
}

fn assert_roundtrip<'a, C>(
    subscriber: &mut Session<'_, 'a, C>,
    publisher: &mut Session<'_, 'a, C>,
    topic: &str,
    payload: &[u8],
) where
    C: minimq::transport::Connector,
{
    assert!(matches!(
        block_on(subscriber.poll()).unwrap(),
        Event::Connected
    ));
    let topics = [TopicFilter::new(topic)];
    block_on(subscriber.subscribe(&topics, &[])).unwrap();
    let _ = poll_until_ready(subscriber, false);

    assert!(matches!(
        block_on(publisher.poll()).unwrap(),
        Event::Connected
    ));
    block_on(publisher.publish(Publication::new(topic, payload).qos(QoS::AtLeastOnce))).unwrap();

    let (received_topic, received_payload, received_qos) =
        poll_until_ready(subscriber, true).expect("publish");
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

    let connector = TcpConnector::new(StdStack::new(IO_TIMEOUT));
    let mut subscriber = Session::new(
        config(Broker::socket_addr(addr), &unique_client_id("sub")),
        &connector,
    );
    let mut publisher = Session::new(
        config(Broker::socket_addr(addr), &unique_client_id("pub")),
        &connector,
    );
    let topic = unique_topic();

    assert_roundtrip(
        &mut subscriber,
        &mut publisher,
        &topic,
        b"hello from minimq",
    );
}

#[test]
fn real_broker_qos1_roundtrip_over_dns_connector() {
    let Some(host) = hostname_broker() else {
        eprintln!("skipping hostname broker test; set {BROKER_HOST_ENV}=hostname");
        return;
    };

    let broker = Broker::hostname(
        &host,
        socket_broker().map(|addr| addr.port()).unwrap_or(1883),
    );
    let connector = DnsTcpConnector::new(StdStack::new(IO_TIMEOUT), StdDns, AddrType::IPv4);
    let mut subscriber = Session::new(config(broker, &unique_client_id("dns-sub")), &connector);
    let mut publisher = Session::new(config(broker, &unique_client_id("dns-pub")), &connector);
    let topic = unique_topic();

    assert_roundtrip(&mut subscriber, &mut publisher, &topic, b"hello over dns");
}
