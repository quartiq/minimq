//! TLS connectivity example for `embedded-tls`.
//!
//! `embedded-tls` does not expose `ReadReady` / `WriteReady`, so this adapter reports
//! unconditional readiness and the resulting `Session::poll()` loop is not bounded.
//! Use this as a TLS transport example, not as a model for a readiness-aware connector.

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::{ErrorKind, ErrorType, Read, ReadReady, Write, WriteReady};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use minimq::{
    Broker, ConfigBuilder, ConnectEvent, Event, Property, Publication, QoS, Session,
    transport::Connector, types::TopicFilter,
};
use std::error::Error as StdError;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use tokio::net::TcpStream;

const BROKER_HOST: &str = "broker.emqx.io";
const BROKER_PORT: u16 = 8883;
const USERNAME: &str = "emqx";
const PASSWORD: &str = "public";
const TLS_READ_RECORD_BUFFER_LEN: usize = 16_640;
const TLS_WRITE_RECORD_BUFFER_LEN: usize = 4_096;

fn kind_from_std(err: &std::io::Error) -> ErrorKind {
    err.kind().into()
}

#[derive(Debug)]
struct EmqxTlsConnector;

#[derive(Debug, Error)]
enum EmqxTlsError {
    #[error("hostname broker is required")]
    UnsupportedBroker,
    #[error("tcp connect error: {0:?}")]
    Tcp(ErrorKind),
    #[error("tls error: {0:?}")]
    Tls(embedded_tls::TlsError),
}

impl embedded_io_async::Error for EmqxTlsError {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::UnsupportedBroker => ErrorKind::Unsupported,
            Self::Tcp(kind) => *kind,
            Self::Tls(err) => embedded_io_async::Error::kind(err),
        }
    }
}

struct EmqxTlsConnection(TlsConnection<'static, FromTokio<TcpStream>, Aes128GcmSha256>);

impl ErrorType for EmqxTlsConnection {
    type Error = EmqxTlsError;
}

impl Read for EmqxTlsConnection {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.0.read(buf).await.map_err(EmqxTlsError::Tls)
    }
}

impl Write for EmqxTlsConnection {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.0.write(buf).await.map_err(EmqxTlsError::Tls)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.0.flush().await.map_err(EmqxTlsError::Tls)
    }
}

impl ReadReady for EmqxTlsConnection {
    fn read_ready(&mut self) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

impl WriteReady for EmqxTlsConnection {
    fn write_ready(&mut self) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

impl Connector for EmqxTlsConnector {
    type Error = EmqxTlsError;
    type Connection<'a> = EmqxTlsConnection;

    async fn connect<'a>(
        &'a self,
        broker: &Broker<'_>,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        let (host, port) = match broker {
            Broker::Hostname { host, port } => (*host, *port),
            Broker::SocketAddr(_) => return Err(EmqxTlsError::UnsupportedBroker),
        };
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|err| EmqxTlsError::Tcp(kind_from_std(&err)))?;
        let read_record_buffer = Box::leak(Box::new([0u8; TLS_READ_RECORD_BUFFER_LEN]));
        let write_record_buffer = Box::leak(Box::new([0u8; TLS_WRITE_RECORD_BUFFER_LEN]));
        let config = TlsConfig::new()
            .with_server_name(host)
            .enable_rsa_signatures();
        let mut tls = TlsConnection::new(
            FromTokio::new(stream),
            read_record_buffer,
            write_record_buffer,
        );
        let mut provider = UnsecureProvider::new::<Aes128GcmSha256>(rand::rngs::OsRng);
        tls.open(TlsContext::new(&config, &mut provider))
            .await
            .map_err(EmqxTlsError::Tls)?;
        Ok(EmqxTlsConnection(tls))
    }
}

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq-{label}-{nanos}")
}

async fn connect(
    session: &mut Session<'_, '_, EmqxTlsConnector>,
) -> Result<(), minimq::SessionError<EmqxTlsConnector>> {
    match tokio::time::timeout(Duration::from_secs(10), session.connect())
        .await
        .unwrap()?
    {
        ConnectEvent::Connected | ConnectEvent::Reconnected => Ok(()),
    }
}

async fn flush(
    session: &mut Session<'_, '_, EmqxTlsConnector>,
) -> Result<(), minimq::SessionError<EmqxTlsConnector>> {
    while !matches!(
        tokio::time::timeout(Duration::from_secs(10), session.poll())
            .await
            .unwrap()?,
        Event::Idle
    ) {}
    Ok(())
}

async fn recv(
    session: &mut Session<'_, '_, EmqxTlsConnector>,
    topic: &str,
    payload: &[u8],
) -> Result<(), minimq::SessionError<EmqxTlsConnector>> {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), session.poll())
            .await
            .unwrap()?
        {
            Event::Inbound(message) if message.topic() == topic && message.payload() == payload => {
                println!(
                    "received topic={} payload={}",
                    message.topic(),
                    payload.escape_ascii()
                );
                return Ok(());
            }
            Event::Inbound(_) | Event::Idle => {}
        }
    }
}

async fn run() -> Result<(), Box<dyn StdError>> {
    let broker = Broker::hostname(BROKER_HOST, BROKER_PORT);
    let connector = EmqxTlsConnector;
    let topic = format!("minimq/examples/tls/{}", unique_id("topic"));
    let payload = format!("hello over tls {}", unique_id("msg"));

    let mut sub_storage = [0u8; 4096];
    let mut pub_storage = [0u8; 4096];

    let mut subscriber = Session::new(
        ConfigBuilder::from_buffer(broker, &mut sub_storage, 1024)?
            .client_id(&unique_id("sub"))?
            .auth(USERNAME, PASSWORD.as_bytes())?,
        &connector,
    );
    let mut publisher = Session::new(
        ConfigBuilder::from_buffer(broker, &mut pub_storage, 1024)?
            .client_id(&unique_id("pub"))?
            .auth(USERNAME, PASSWORD.as_bytes())?,
        &connector,
    );

    connect(&mut subscriber).await?;
    subscriber
        .subscribe(&[TopicFilter::new(&topic)], &[] as &[Property<'_>])
        .await?;
    flush(&mut subscriber).await?;

    connect(&mut publisher).await?;
    publisher
        .publish(Publication::new(&topic, payload.as_bytes()).qos(QoS::AtLeastOnce))
        .await?;
    flush(&mut publisher).await?;

    recv(&mut subscriber, &topic, payload.as_bytes()).await?;
    Ok(())
}

#[tokio::main]
async fn main() {
    env_logger::init();
    run().await.unwrap();
}
