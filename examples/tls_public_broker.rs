use embedded_io_adapters::tokio_1::FromTokio;
use embedded_io_async::Error as _;
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use minimq::{
    Broker, BufferLayout, ConfigBuilder, Event, Property, Publication, QoS, Session,
    transport::Connector, types::TopicFilter,
};
use std::fmt::{Display, Formatter};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;

const BROKER_HOST: &str = "broker.emqx.io";
const BROKER_PORT: u16 = 8883;
const USERNAME: &str = "emqx";
const PASSWORD: &str = "public";

fn kind_from_std(err: &std::io::Error) -> minimq::embedded_io_async::ErrorKind {
    err.kind().into()
}

#[derive(Debug)]
enum ExampleError {
    Config(minimq::ConfigError),
    Protocol(minimq::ProtocolError),
    Session(minimq::Error),
    Publish(minimq::PubError<()>),
    Unexpected(&'static str),
    Timeout,
}

impl Display for ExampleError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Config(err) => write!(f, "{err:?}"),
            Self::Protocol(err) => write!(f, "{err:?}"),
            Self::Session(err) => write!(f, "{err:?}"),
            Self::Publish(err) => write!(f, "{err:?}"),
            Self::Unexpected(what) => write!(f, "unexpected event: {what}"),
            Self::Timeout => write!(f, "timed out waiting for subscribed publish"),
        }
    }
}

impl std::error::Error for ExampleError {}

impl From<minimq::ConfigError> for ExampleError {
    fn from(err: minimq::ConfigError) -> Self {
        Self::Config(err)
    }
}

impl From<minimq::ProtocolError> for ExampleError {
    fn from(err: minimq::ProtocolError) -> Self {
        Self::Protocol(err)
    }
}

impl From<minimq::Error> for ExampleError {
    fn from(err: minimq::Error) -> Self {
        Self::Session(err)
    }
}

impl From<minimq::PubError<()>> for ExampleError {
    fn from(err: minimq::PubError<()>) -> Self {
        Self::Publish(err)
    }
}

struct EmqxTlsConnector;

impl Connector for EmqxTlsConnector {
    type Error = embedded_tls::TlsError;
    type Connection<'a> = TlsConnection<'static, FromTokio<TcpStream>, Aes128GcmSha256>;

    async fn connect<'a>(
        &'a self,
        broker: &Broker<'_>,
    ) -> Result<Self::Connection<'a>, minimq::Error> {
        let (host, port) = match broker {
            Broker::Hostname { host, port } => (*host, *port),
            Broker::SocketAddr(_) => {
                return Err(minimq::Error::Transport(
                    minimq::embedded_io_async::ErrorKind::Unsupported,
                ));
            }
        };
        let stream = TcpStream::connect((host, port))
            .await
            .map_err(|err| minimq::Error::Transport(kind_from_std(&err)))?;
        let read_record_buffer = Box::leak(Box::new([0u8; 16_384]));
        let write_record_buffer = Box::leak(Box::new([0u8; 4_096]));
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
            .map_err(|err| minimq::Error::Transport(err.kind()))?;
        Ok(tls)
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

    let sub_config = ConfigBuilder::from_buffer_layout(
        broker,
        &mut sub_storage,
        BufferLayout { rx: 1024, tx: 3072 },
    )?
    .client_id(&unique_id("sub"))?
    .set_auth(USERNAME, PASSWORD)?
    .build();

    let pub_config = ConfigBuilder::from_buffer_layout(
        broker,
        &mut pub_storage,
        BufferLayout { rx: 1024, tx: 3072 },
    )?
    .client_id(&unique_id("pub"))?
    .set_auth(USERNAME, PASSWORD)?
    .build();

    let mut subscriber = Session::new(sub_config, &connector);
    let mut publisher = Session::new(pub_config, &connector);

    match subscriber.poll().await? {
        Event::Connected => {}
        _ => return Err(ExampleError::Unexpected("subscriber connect")),
    }
    subscriber
        .subscribe(&[TopicFilter::new(&topic)], &[] as &[Property<'_>])
        .await?;
    let _ = subscriber.poll().await?;

    match publisher.poll().await? {
        Event::Connected => {}
        _ => return Err(ExampleError::Unexpected("publisher connect")),
    }
    publisher
        .publish(Publication::new(&topic, payload.as_bytes()).qos(QoS::AtLeastOnce))
        .await?;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(ExampleError::Timeout);
        }
        match subscriber.poll().await? {
            Event::Inbound(message)
                if message.topic == topic && message.payload == payload.as_bytes() =>
            {
                println!("received topic={} payload={}", message.topic, payload);
                return Ok(());
            }
            Event::Inbound(_) | Event::Idle | Event::Connected | Event::Reconnected => {}
        }
    }
}
