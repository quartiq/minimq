//! MQTT v5 request/reply echo demonstrating session resumption over `embedded-tls`.

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use minimq::{ConfigBuilder, ConnectEvent, Session, TopicFilter};
use std::error::Error as StdError;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;

const BROKER_HOST: &str = "broker.emqx.io";
const BROKER_PORT: u16 = 8883;
const USERNAME: &str = "emqx";
const PASSWORD: &str = "public";
const TLS_READ_RECORD_BUFFER_LEN: usize = 16_640;
const TLS_WRITE_RECORD_BUFFER_LEN: usize = 4_096;

async fn connect_tls<'a>(
    host: &str,
    port: u16,
    read_record: &'a mut [u8],
    write_record: &'a mut [u8],
) -> Result<TlsConnection<'a, FromTokio<TcpStream>, Aes128GcmSha256>, Box<dyn StdError>> {
    let config = TlsConfig::new()
        .with_server_name(host)
        .enable_rsa_signatures();
    let mut tls = TlsConnection::new(
        FromTokio::new(TcpStream::connect((host, port)).await?),
        read_record,
        write_record,
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
    let client_id = unique_id("client");
    let topic = format!("minimq/examples/echo/{}", unique_id("request"));
    let mut read_record = vec![0; TLS_READ_RECORD_BUFFER_LEN];
    let mut write_record = vec![0; TLS_WRITE_RECORD_BUFFER_LEN];
    let mut storage = [0u8; 2048];
    let mut session = Session::new(
        ConfigBuilder::from_buffer(&mut storage, 1024)?
            .client_id(&client_id)?
            .auth(USERNAME, PASSWORD.as_bytes())?
            .session_expiry_interval(60),
    );

    let tls = connect_tls(
        BROKER_HOST,
        BROKER_PORT,
        &mut read_record,
        &mut write_record,
    )
    .await?;
    let mut connection = session.connect(tls).await?;
    let subscribe = connection
        .subscribe(&[TopicFilter::new(&topic)], &[])
        .await?;
    while connection.is_pending(&subscribe) {
        connection.poll().await?;
    }
    println!("echoing requests on {topic}");

    loop {
        let (reply, payload) = {
            let request = connection.recv().await?;
            // Preserve the MQTT v5 response topic and correlation data past this receive borrow.
            let Some(reply) = request.reply_owned::<256, 64>()? else {
                continue;
            };
            (reply, request.payload().to_vec())
        };

        connection.disconnect().await?;
        drop(connection);
        let tls = connect_tls(
            BROKER_HOST,
            BROKER_PORT,
            &mut read_record,
            &mut write_record,
        )
        .await?;
        connection = session.connect(tls).await?;
        if connection.connect_event() != ConnectEvent::Reconnected {
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
    defmt2log::init_from_current_exe().unwrap();
    run().await.unwrap();
}
