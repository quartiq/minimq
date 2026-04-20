use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use minimq::{
    Broker, ConfigBuilder, Event, Property, Publication, QoS, Session, transport::Connector,
    types::TopicFilter,
};
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
enum ExampleError {
    #[error(transparent)]
    Setup(#[from] minimq::SetupError),
    #[error(transparent)]
    Protocol(#[from] minimq::ProtocolError),
    #[error(transparent)]
    Session(#[from] minimq::SessionError<EmqxTlsConnector>),
    #[error(transparent)]
    Publish(#[from] minimq::PublishError<EmqxTlsConnector, ()>),
    #[error("unexpected event: {0}")]
    Unexpected(&'static str),
    #[error("timed out waiting for subscribed publish")]
    Timeout,
}

async fn poll_until_idle(
    session: &mut Session<'_, '_, EmqxTlsConnector>,
) -> Result<(), ExampleError> {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), session.poll())
            .await
            .map_err(|_| ExampleError::Timeout)??
        {
            Event::Idle => return Ok(()),
            Event::Connected | Event::Reconnected | Event::Inbound(_) => {}
        }
    }
}

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

#[tokio::main]
async fn main() -> Result<(), ExampleError> {
    env_logger::init();

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

    match tokio::time::timeout(Duration::from_secs(10), subscriber.poll())
        .await
        .map_err(|_| ExampleError::Timeout)??
    {
        Event::Connected => {}
        _ => return Err(ExampleError::Unexpected("subscriber connect")),
    }
    subscriber
        .subscribe(&[TopicFilter::new(&topic)], &[] as &[Property<'_>])
        .await?;
    let _ = tokio::time::timeout(Duration::from_secs(10), subscriber.poll())
        .await
        .map_err(|_| ExampleError::Timeout)??;

    match tokio::time::timeout(Duration::from_secs(10), publisher.poll())
        .await
        .map_err(|_| ExampleError::Timeout)??
    {
        Event::Connected => {}
        _ => return Err(ExampleError::Unexpected("publisher connect")),
    }
    publisher
        .publish(Publication::new(&topic, payload.as_bytes()).qos(QoS::AtLeastOnce))
        .await?;
    poll_until_idle(&mut publisher).await?;

    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match subscriber.poll().await? {
                Event::Inbound(message)
                    if message.topic() == topic && message.payload() == payload.as_bytes() =>
                {
                    println!("received topic={} payload={}", message.topic(), payload);
                    return Ok(());
                }
                Event::Inbound(_) | Event::Idle | Event::Connected | Event::Reconnected => {}
            }
        }
    })
    .await
    .map_err(|_| ExampleError::Timeout)?
}
