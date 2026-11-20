//! MQTT v5 request/reply echo demonstrating session resumption over TLS.

use embedded_io_adapters::tokio_1::FromTokio;
use minimq::{ConfigBuilder, ConnectEvent, Session, TopicFilter};
use std::error::Error as StdError;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio_rustls::{
    TlsConnector,
    client::TlsStream,
    rustls::{ClientConfig, RootCertStore, pki_types::ServerName},
};

const BROKER_HOST: &str = "broker.emqx.io";
const BROKER_PORT: u16 = 8883;
const USERNAME: &str = "emqx";
const PASSWORD: &str = "public";

async fn connect_tls(
    host: &str,
    port: u16,
    connector: &TlsConnector,
) -> Result<FromTokio<TlsStream<TcpStream>>, Box<dyn StdError>> {
    let socket = TcpStream::connect((host, port)).await?;
    let tls = connector
        .connect(ServerName::try_from(host.to_owned())?, socket)
        .await?;
    Ok(FromTokio::new(tls))
}

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq-{label}-{nanos}")
}

async fn run() -> Result<(), Box<dyn StdError>> {
    let client_id = unique_id("client");
    let topic = format!("minimq/examples/echo/{}", unique_id("request"));
    let roots = RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let connector = TlsConnector::from(Arc::new(config));
    let mut storage = [0u8; 2048];
    let mut session = Session::new(
        ConfigBuilder::from_buffer(&mut storage, 1024)?
            .client_id(&client_id)?
            .auth(USERNAME, PASSWORD.as_bytes())?
            .session_expiry_interval(60),
    );

    let tls = connect_tls(BROKER_HOST, BROKER_PORT, &connector).await?;
    let mut connection = session.connect(tls).await?;
    let subscribe = connection
        .subscribe(&[TopicFilter::new(&topic)], &[])
        .await?;
    while connection.is_pending(&subscribe) {
        if let minimq::Event::Completed { op, outcome } = connection.poll().await?
            && op == subscribe
        {
            outcome.check()?;
        }
    }
    println!("echoing requests on {topic}");

    loop {
        let (reply, payload) = {
            let minimq::Event::Publish(request) = connection.poll().await? else {
                continue;
            };
            // Preserve the MQTT v5 response topic and correlation data past this receive borrow.
            let Some(reply) = request.reply_owned::<256, 64>()? else {
                continue;
            };
            (reply, request.payload().to_vec())
        };

        connection.disconnect().await?;
        drop(connection);
        let tls = connect_tls(BROKER_HOST, BROKER_PORT, &connector).await?;
        connection = session.connect(tls).await?;
        if connection.connect_event() != ConnectEvent::SessionResumed {
            return Err("broker did not resume the MQTT session".into());
        }
        connection
            .publish(reply.publication(payload.as_slice()))
            .await?;
    }
}

#[tokio::main]
async fn main() {
    env_logger::init();
    defmt2log::init_from_current_exe();
    run().await.unwrap();
}
