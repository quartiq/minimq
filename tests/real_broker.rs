use core::future::poll_fn;
use core::pin::Pin;
use core::task::Poll;
use embedded_io_async::{Error as _, ErrorKind, ErrorType, Read, ReadReady, Write, WriteReady};
use minimq::{
    Broker, Buffers, ConfigBuilder, ConnectEvent, Error, Event, Publication, QoS, Session,
    transport::Connector,
    types::{SubscriptionOptions, TopicFilter},
};
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, lookup_host},
};

const BROKER_ADDR_ENV: &str = "MINIMQ_REAL_BROKER_ADDR";
const BROKER_HOST_ENV: &str = "MINIMQ_REAL_BROKER_HOST";

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

fn config<'a>(broker: Broker<'a>, client_id: &str) -> ConfigBuilder<'a> {
    let rx = Box::leak(Box::new([0; 1024]));
    let tx = Box::leak(Box::new([0; 2048]));
    ConfigBuilder::new(broker, Buffers::new(rx, tx))
        .client_id(client_id)
        .unwrap()
}

struct TokioConnection(TcpStream);

impl ErrorType for TokioConnection {
    type Error = std::io::Error;
}

impl Read for TokioConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }
        poll_fn(|cx| {
            let mut read_buf = tokio::io::ReadBuf::new(buf);
            match Pin::new(&mut self.0).poll_read(cx, &mut read_buf) {
                Poll::Ready(Ok(())) => Poll::Ready(Ok(read_buf.filled().len())),
                Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
                Poll::Pending => Poll::Pending,
            }
        })
        .await
    }
}

impl Write for TokioConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        poll_fn(|cx| match Pin::new(&mut self.0).poll_write(cx, buf) {
            Poll::Ready(Ok(0)) if !buf.is_empty() => {
                Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()))
            }
            Poll::Ready(result) => Poll::Ready(result),
            Poll::Pending => Poll::Pending,
        })
        .await
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        poll_fn(|cx| Pin::new(&mut self.0).poll_flush(cx)).await
    }
}

impl ReadReady for TokioConnection {
    fn read_ready(&mut self) -> Result<bool, Self::Error> {
        match self.0.try_io(tokio::io::Interest::READABLE, || Ok(())) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(err) => Err(err),
        }
    }
}

impl WriteReady for TokioConnection {
    fn write_ready(&mut self) -> Result<bool, Self::Error> {
        match self.0.try_io(tokio::io::Interest::WRITABLE, || Ok(())) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(false),
            Err(err) => Err(err),
        }
    }
}

#[derive(Copy, Clone)]
struct TokioConnector;

impl Connector for TokioConnector {
    type Error = std::io::Error;
    type Connection<'a> = TokioConnection;

    async fn connect<'a>(
        &'a self,
        broker: &Broker<'_>,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let addr = match broker {
            Broker::SocketAddr(addr) => *addr,
            Broker::Hostname { host, port } => lookup_host((*host, *port))
                .await?
                .next()
                .ok_or(std::io::ErrorKind::NotFound)?,
        };
        TcpStream::connect(addr).await.map(TokioConnection)
    }
}

async fn poll_until_ready<'a, C>(
    session: &mut Session<'_, 'a, C>,
    want_inbound: bool,
) -> Option<(String, Vec<u8>, QoS)>
where
    C: Connector,
    C::Error: embedded_io_async::Error + core::fmt::Debug,
{
    for _ in 0..200 {
        match session.poll().await {
            Ok(Event::Idle) if !want_inbound => {
                return None;
            }
            Ok(Event::Inbound(message)) if want_inbound => {
                return Some((
                    message.topic().to_string(),
                    message.payload().to_vec(),
                    message.qos(),
                ));
            }
            Ok(_) => {}
            Err(Error::Transport(err)) => match err.kind() {
                ErrorKind::TimedOut | ErrorKind::Interrupted => {}
                _ => panic!("session poll failed: {err:?}"),
            },
            Err(err) => panic!("session poll failed: {err:?}"),
        }
    }
    panic!("timed out waiting for broker activity");
}

async fn assert_roundtrip<'a, C>(
    subscriber: &mut Session<'_, 'a, C>,
    publisher: &mut Session<'_, 'a, C>,
    topic: &str,
    payload: &[u8],
) where
    C: Connector,
    C::Error: embedded_io_async::Error + core::fmt::Debug,
{
    assert!(matches!(
        subscriber.connect().await.unwrap(),
        ConnectEvent::Connected | ConnectEvent::Reconnected
    ));
    let topics = [TopicFilter::new(topic)
        .options(SubscriptionOptions::default().maximum_qos(QoS::AtLeastOnce))];
    subscriber.subscribe(&topics, &[]).await.unwrap();
    let _ = poll_until_ready(subscriber, false).await;

    assert!(matches!(
        publisher.connect().await.unwrap(),
        ConnectEvent::Connected | ConnectEvent::Reconnected
    ));
    publisher
        .publish(Publication::bytes(topic, payload).qos(QoS::AtLeastOnce))
        .await
        .unwrap();

    let (received_topic, received_payload, received_qos) =
        poll_until_ready(subscriber, true).await.expect("publish");
    assert_eq!(received_topic, topic);
    assert_eq!(received_payload, payload);
    assert_eq!(received_qos, QoS::AtLeastOnce);
}

#[tokio::test]
async fn real_broker_qos1_roundtrip_over_tcp_connector() {
    let Some(addr) = socket_broker() else {
        eprintln!("skipping real broker test; set {BROKER_ADDR_ENV}=host:port");
        return;
    };

    let connector = TokioConnector;
    let mut subscriber = Session::new(config(addr.into(), &unique_client_id("sub")), &connector);
    let mut publisher = Session::new(config(addr.into(), &unique_client_id("pub")), &connector);
    let topic = unique_topic();

    assert_roundtrip(
        &mut subscriber,
        &mut publisher,
        &topic,
        b"hello from minimq",
    )
    .await;
}

#[tokio::test]
async fn real_broker_qos1_roundtrip_over_dns_connector() {
    let Some(host) = hostname_broker() else {
        eprintln!("skipping hostname broker test; set {BROKER_HOST_ENV}=hostname");
        return;
    };

    let broker = Broker::hostname(
        &host,
        socket_broker().map(|addr| addr.port()).unwrap_or(1883),
    );
    let connector = TokioConnector;
    let mut subscriber = Session::new(config(broker, &unique_client_id("dns-sub")), &connector);
    let mut publisher = Session::new(config(broker, &unique_client_id("dns-pub")), &connector);
    let topic = unique_topic();

    assert_roundtrip(&mut subscriber, &mut publisher, &topic, b"hello over dns").await;
}
