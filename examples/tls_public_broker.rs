//! TLS connectivity example for `embedded-tls`.
//!
//! Use this as a TLS transport example. Bounded/cooperative session driving is done by wrapping
//! the cancel-safe blocking `Session::poll()` in an external timeout at the call site.

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use minimq::{ConfigBuilder, OpStatus, Publication, QoS, Session, TopicFilter};
use std::error::Error as StdError;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;

const BROKER_HOST: &str = "broker.emqx.io";
const BROKER_PORT: u16 = 8883;
const USERNAME: &str = "emqx";
const PASSWORD: &str = "public";
const TLS_READ_RECORD_BUFFER_LEN: usize = 16_640;
const TLS_WRITE_RECORD_BUFFER_LEN: usize = 4_096;

async fn connect_tls(
    host: &str,
    port: u16,
) -> Result<TlsConnection<'static, FromTokio<TcpStream>, Aes128GcmSha256>, Box<dyn StdError>> {
    let config = TlsConfig::new()
        .with_server_name(host)
        .enable_rsa_signatures();
    let mut tls = TlsConnection::new(
        FromTokio::new(TcpStream::connect((host, port)).await?),
        Box::leak(Box::new([0u8; TLS_READ_RECORD_BUFFER_LEN])),
        Box::leak(Box::new([0u8; TLS_WRITE_RECORD_BUFFER_LEN])),
    );
    let mut provider = UnsecureProvider::new::<Aes128GcmSha256>(rand::rngs::OsRng);
    tls.open(TlsContext::new(&config, &mut provider)).await?;
    Ok(tls)
}

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq-{label}-{nanos}")
}

async fn run() -> Result<(), Box<dyn StdError>> {
    let topic = format!("minimq/examples/tls/{}", unique_id("topic"));
    let payload_str = format!("hello over tls {}", unique_id("msg"));
    let payload = payload_str.as_bytes();

    let mut sub_storage = [0u8; 2048];
    let mut subscriber = Session::new(
        ConfigBuilder::from_buffer(&mut sub_storage, 1024)?.auth(USERNAME, PASSWORD.as_bytes())?,
    );
    subscriber
        .connect(connect_tls(BROKER_HOST, BROKER_PORT).await?)
        .await?;
    let sub = subscriber
        .subscribe(&[TopicFilter::new(&topic)], &[])
        .await?;
    while subscriber.status(&sub) == OpStatus::Pending {
        subscriber.poll().await?;
    }

    let mut pub_storage = [0u8; 2048];
    let mut publisher = Session::new(
        ConfigBuilder::from_buffer(&mut pub_storage, 1024)?.auth(USERNAME, PASSWORD.as_bytes())?,
    );
    publisher
        .connect(connect_tls(BROKER_HOST, BROKER_PORT).await?)
        .await?;
    let pub_ = publisher
        .publish(Publication::new(&topic, payload).qos(QoS::AtLeastOnce))
        .await?
        .unwrap();
    while publisher.status(&pub_) == OpStatus::Pending {
        publisher.poll().await?;
    }

    loop {
        let message = subscriber.recv().await?;
        if message.topic() == topic && message.payload() == payload {
            println!(
                "received topic={} payload={}",
                message.topic(),
                payload.escape_ascii()
            );
            return Ok(());
        }
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    defmt2log::init_from_current_exe().unwrap();
    run().await.unwrap();
}
