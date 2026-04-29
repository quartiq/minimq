//! TLS connectivity example for `embedded-tls`.
//!
//! Use this as a TLS transport example. Bounded/cooperative session driving is done by wrapping
//! the cancel-safe blocking `Session::poll()` in an external timeout at the call site.

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use minimq::{
    ConfigBuilder, ConnectEvent, Property, Publication, QoS, Session, types::TopicFilter,
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

#[derive(Debug, Error)]
enum EmqxTlsError {
    #[error("tcp connect error: {0:?}")]
    Tcp(ErrorKind),
    #[error("tls error: {0:?}")]
    Tls(embedded_tls::TlsError),
}

impl embedded_io_async::Error for EmqxTlsError {
    fn kind(&self) -> ErrorKind {
        match self {
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

async fn connect_tls(host: &str, port: u16) -> Result<EmqxTlsConnection, EmqxTlsError> {
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

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq-{label}-{nanos}")
}

async fn connect(
    session: &mut Session<'_, EmqxTlsConnection>,
    io: EmqxTlsConnection,
) -> Result<(), minimq::SessionError<EmqxTlsConnection>> {
    match tokio::time::timeout(Duration::from_secs(10), session.connect(io))
        .await
        .unwrap()?
    {
        ConnectEvent::Connected | ConnectEvent::Reconnected => Ok(()),
    }
}

async fn flush(
    session: &mut Session<'_, EmqxTlsConnection>,
) -> Result<(), minimq::SessionError<EmqxTlsConnection>> {
    while !session.is_publish_quiescent() {
        match tokio::time::timeout(Duration::from_millis(10), session.poll()).await {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => return Err(err),
            Err(_) => {}
        }
    }
    Ok(())
}

async fn recv(
    session: &mut Session<'_, EmqxTlsConnection>,
    topic: &str,
    payload: &[u8],
) -> Result<(), minimq::SessionError<EmqxTlsConnection>> {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), session.poll())
            .await
            .unwrap()?
        {
            message if message.topic() == topic && message.payload() == payload => {
                println!(
                    "received topic={} payload={}",
                    message.topic(),
                    payload.escape_ascii()
                );
                return Ok(());
            }
            _ => {}
        }
    }
}

async fn run() -> Result<(), Box<dyn StdError>> {
    let topic = format!("minimq/examples/tls/{}", unique_id("topic"));
    let payload = format!("hello over tls {}", unique_id("msg"));

    let mut sub_storage = [0u8; 4096];
    let mut pub_storage = [0u8; 4096];

    let mut subscriber = Session::new(
        ConfigBuilder::from_buffer(&mut sub_storage, 1024)?
            .client_id(&unique_id("sub"))?
            .auth(USERNAME, PASSWORD.as_bytes())?,
    );
    let mut publisher = Session::new(
        ConfigBuilder::from_buffer(&mut pub_storage, 1024)?
            .client_id(&unique_id("pub"))?
            .auth(USERNAME, PASSWORD.as_bytes())?,
    );

    connect(
        &mut subscriber,
        connect_tls(BROKER_HOST, BROKER_PORT).await?,
    )
    .await?;
    subscriber
        .subscribe(&[TopicFilter::new(&topic)], &[] as &[Property<'_>])
        .await?;
    flush(&mut subscriber).await?;

    connect(&mut publisher, connect_tls(BROKER_HOST, BROKER_PORT).await?).await?;
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
