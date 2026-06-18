//! TLS connectivity example for `embedded-tls`.
//!
//! It also showcases the connection-handle API: the transport lives in the
//! [`Connection`](minimq::Connection) handle returned by [`Session::connect`], not in the
//! `Session`, so the TLS record buffers only need to live as long as one connection. A single pair
//! of record buffers is owned here and re-lent to every connection, so one allocation serves every
//! reconnect.

use embedded_io_adapters::tokio_1::FromTokio;
use embedded_tls::{Aes128GcmSha256, TlsConfig, TlsConnection, TlsContext, UnsecureProvider};
use minimq::{
    ConfigBuilder, ConnectEvent, Connection, Error, Io, PubError, Publication, QoS, Session,
    TopicFilter,
};
use std::error::Error as StdError;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;

const BROKER_HOST: &str = "broker.emqx.io";
const BROKER_PORT: u16 = 8883;
const USERNAME: &str = "emqx";
const PASSWORD: &str = "public";
const TLS_READ_RECORD_BUFFER_LEN: usize = 16_640;
const TLS_WRITE_RECORD_BUFFER_LEN: usize = 4_096;

/// Number of successful round-trips to perform before exiting. A real long-running client would
/// loop forever; we stop after a few so the example terminates while still reconnecting (and so
/// reusing the record buffers) more than once.
const ROUND_TRIPS: usize = 2;

/// Open a TLS connection over the caller-owned record buffers.
///
/// The returned connection borrows `read_record`/`write_record` for its own lifetime `'a`, not
/// `'static`, which is what lets the caller reuse the buffers once the connection is dropped.
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

/// Subscribe, publish one message, and wait to receive it back over the same connection.
///
/// Errors are returned rather than handled here so the caller can decide whether a failure means
/// "reconnect" or "give up".
async fn round_trip<C: Io>(
    conn: &mut Connection<'_, '_, C>,
    topic: &str,
    payload: &[u8],
) -> Result<(), Error<C::Error>> {
    // A resumed session keeps its broker-side subscriptions, so only (re)subscribe on a session
    // the broker reports as fresh.
    if matches!(conn.connect_event(), ConnectEvent::Connected) {
        let sub = conn.subscribe(&[TopicFilter::new(topic)], &[]).await?;
        while conn.is_pending(&sub) {
            conn.poll().await?;
        }
    }

    let op = match conn
        .publish(Publication::new(topic, payload).qos(QoS::AtLeastOnce))
        .await
    {
        Ok(op) => op.expect("a QoS 1 publish yields an operation handle"),
        Err(PubError::Session(err)) => return Err(err),
        // A `&[u8]` payload is copied verbatim and cannot fail to serialize.
        Err(PubError::Payload(())) => unreachable!("byte-slice payloads are infallible"),
    };
    while conn.is_pending(&op) {
        conn.poll().await?;
    }

    loop {
        let message = conn.recv().await?;
        if message.topic() == topic && message.payload() == payload {
            return Ok(());
        }
    }
}

async fn run() -> Result<(), Box<dyn StdError>> {
    let topic = format!("minimq/examples/tls/{}", unique_id("topic"));

    // Owned once and re-lent to every connection below: one pair of record buffers serves every
    // reconnect, with no `Box::leak` and no per-connection allocation.
    let mut read_record = vec![0u8; TLS_READ_RECORD_BUFFER_LEN];
    let mut write_record = vec![0u8; TLS_WRITE_RECORD_BUFFER_LEN];

    let mut session_storage = [0u8; 2048];
    let mut session = Session::new(
        ConfigBuilder::from_buffer(&mut session_storage, 1024)?
            .auth(USERNAME, PASSWORD.as_bytes())?,
    );

    // The correct shape for an MQTT client is an outer loop that reconnects instead of bailing out
    // when a connection drops. Every iteration borrows the same record buffers for the new
    // connection.
    let mut completed = 0;
    while completed < ROUND_TRIPS {
        let tls = match connect_tls(
            BROKER_HOST,
            BROKER_PORT,
            &mut read_record[..],
            &mut write_record[..],
        )
        .await
        {
            Ok(tls) => tls,
            Err(err) => {
                eprintln!("transport connect failed ({err}); retrying");
                continue;
            }
        };

        let mut conn = match session.connect(tls).await {
            Ok(conn) => conn,
            Err(err) => {
                eprintln!("MQTT connect failed ({err}); retrying");
                continue;
            }
        };

        let payload = format!("hello over tls #{completed}");
        match round_trip(&mut conn, &topic, payload.as_bytes()).await {
            Ok(()) => {
                println!("round-trip {completed} ok");
                completed += 1;

                // Close gracefully: this sends an MQTT DISCONNECT, so the broker tears the session
                // down cleanly and does not publish the Will. Just dropping `conn` here instead
                // would be an abnormal close — the broker would treat it as a lost connection and
                // publish the configured Will, if any.
                conn.disconnect().await?;
            }
            // Losing the connection mid-exchange is a normal operational event: reconnect and reuse
            // the same record buffers. Letting `conn` drop on the way out of this arm is correct —
            // there is nothing left to send on a dead transport.
            Err(Error::Disconnected) | Err(Error::Transport(_)) => {
                eprintln!("connection lost mid-exchange; reconnecting");
            }
            Err(other) => return Err(other.into()),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() {
    env_logger::init();
    defmt2log::init_from_current_exe().unwrap();
    run().await.unwrap();
}
