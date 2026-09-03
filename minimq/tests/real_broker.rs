mod support;

use core::future::poll_fn;
use core::pin::Pin;
use core::task::Poll;
use embassy_time::{Duration, with_timeout};
use embedded_io_async::{ErrorType, Read, Write};
use minimq::{
    Buffers, ConfigBuilder, ConnectEvent, Connection, Error, Publication, QoS, Session,
    SubscriptionOptions, TopicFilter,
};
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};
use support::init_host_logging;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, lookup_host},
};

const BROKER_ADDR_ENV: &str = "MINIMQ_REAL_BROKER_ADDR";
const BROKER_HOST_ENV: &str = "MINIMQ_REAL_BROKER_HOST";

type Conn<'a, 'buf> = Connection<'a, 'buf, TokioConnection>;

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

fn config(client_id: &str) -> ConfigBuilder<'static> {
    let rx = Box::leak(Box::new([0; 1024]));
    let tx = Box::leak(Box::new([0; 2048]));
    ConfigBuilder::new(Buffers::new(rx, tx))
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

async fn connect_addr(addr: SocketAddr) -> std::io::Result<TokioConnection> {
    TcpStream::connect(addr).await.map(TokioConnection)
}

async fn connect_host(host: &str, port: u16) -> std::io::Result<TokioConnection> {
    let addr = lookup_host((host, port))
        .await?
        .next()
        .ok_or(std::io::ErrorKind::NotFound)?;
    connect_addr(addr).await
}

async fn poll_until_ready(
    conn: &mut Conn<'_, '_>,
    want_inbound: bool,
) -> Option<(String, Vec<u8>, QoS)> {
    for _ in 0..200 {
        let result = if want_inbound {
            Ok(conn.poll().await)
        } else {
            with_timeout(Duration::from_millis(0), conn.poll()).await
        };
        match result {
            Ok(Ok(Some(message))) if want_inbound => {
                return Some((
                    message.topic().to_string(),
                    message.payload().to_vec(),
                    message.qos(),
                ));
            }
            Ok(Ok(None)) => {}
            Err(_) if !want_inbound => return None,
            Ok(Err(Error::Transport(err))) => match err.kind() {
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::Interrupted => {}
                _ => panic!("session poll failed: {err:?}"),
            },
            Ok(Err(err)) => panic!("session poll failed: {err:?}"),
            Ok(Ok(_)) => {}
            Err(_) => {}
        }
    }
    panic!("timed out waiting for broker activity");
}

async fn assert_roundtrip(
    subscriber: &mut Session<'_>,
    publisher: &mut Session<'_>,
    subscriber_io: TokioConnection,
    publisher_io: TokioConnection,
    topic: &str,
    payload: &[u8],
) {
    let mut sub_conn = subscriber.connect(subscriber_io).await.unwrap();
    assert!(matches!(
        sub_conn.connect_event(),
        ConnectEvent::Connected | ConnectEvent::Reconnected
    ));
    let topics = [TopicFilter::new(topic)
        .options(SubscriptionOptions::default().maximum_qos(QoS::AtLeastOnce))];
    sub_conn.subscribe(&topics, &[]).await.unwrap();
    let _ = poll_until_ready(&mut sub_conn, false).await;

    let mut pub_conn = publisher.connect(publisher_io).await.unwrap();
    assert!(matches!(
        pub_conn.connect_event(),
        ConnectEvent::Connected | ConnectEvent::Reconnected
    ));
    pub_conn
        .publish(Publication::bytes(topic, payload).qos(QoS::AtLeastOnce))
        .await
        .unwrap();

    let (received_topic, received_payload, received_qos) = poll_until_ready(&mut sub_conn, true)
        .await
        .expect("publish");
    assert_eq!(received_topic, topic);
    assert_eq!(received_payload, payload);
    assert_eq!(received_qos, QoS::AtLeastOnce);
}

#[tokio::test]
async fn real_broker_qos1_roundtrip_over_tcp() {
    init_host_logging();
    let Some(addr) = socket_broker() else {
        eprintln!("skipping real broker test; set {BROKER_ADDR_ENV}=host:port");
        return;
    };

    let mut subscriber = Session::new(config(&unique_client_id("sub")));
    let mut publisher = Session::new(config(&unique_client_id("pub")));
    let topic = unique_topic();

    assert_roundtrip(
        &mut subscriber,
        &mut publisher,
        connect_addr(addr).await.unwrap(),
        connect_addr(addr).await.unwrap(),
        &topic,
        b"hello from minimq",
    )
    .await;
}

#[tokio::test]
async fn real_broker_qos1_roundtrip_over_dns() {
    init_host_logging();
    let Some(host) = hostname_broker() else {
        eprintln!("skipping hostname broker test; set {BROKER_HOST_ENV}=hostname");
        return;
    };

    let port = socket_broker().map(|addr| addr.port()).unwrap_or(1883);
    let mut subscriber = Session::new(config(&unique_client_id("dns-sub")));
    let mut publisher = Session::new(config(&unique_client_id("dns-pub")));
    let topic = unique_topic();

    assert_roundtrip(
        &mut subscriber,
        &mut publisher,
        connect_host(&host, port).await.unwrap(),
        connect_host(&host, port).await.unwrap(),
        &topic,
        b"hello over dns",
    )
    .await;
}
