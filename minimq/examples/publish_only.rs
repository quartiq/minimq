//! QoS 0 publication with output deadlines and keepalive between samples.
//! Run against a local broker: cargo run -p minimq --example publish_only

#[path = "support/write_timeout.rs"]
mod write_timeout;

use embedded_io_adapters::tokio_1::FromTokio;
use minimq::{ConfigBuilder, Publication, Session};
use std::error::Error;
use tokio::{net::TcpStream, time::Duration};
use write_timeout::WriteTimeout;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let broker = std::env::var("BROKER").unwrap_or_else(|_| "127.0.0.1:1883".into());
    let mut storage = [0; 1024];
    let mut session = Session::new(
        ConfigBuilder::from_buffer(&mut storage, 512)?
            .client_id("minimq-publish-only")?
            .keepalive_interval(10),
    );
    loop {
        // A setup deadline is safe: cancelling connect drops the supplied transport.
        let result: Result<(), Box<dyn Error>> = async {
            let io = WriteTimeout {
                io: FromTokio::new(TcpStream::connect(&broker).await?),
                timeout: embassy_time::Duration::from_secs(5),
            };
            let mut connection =
                tokio::time::timeout(Duration::from_secs(10), session.connect(io)).await??;
            loop {
                // Await QoS 0 completely. A write/flush timeout becomes a transport error;
                // returning drops this connection before the outer loop reconnects.
                connection
                    .publish(Publication::bytes("minimq/example/heartbeat", b"alive"))
                    .await?;
                let next_sample = tokio::time::sleep(Duration::from_secs(30));
                tokio::pin!(next_sample);
                loop {
                    tokio::select! {
                        _ = &mut next_sample => break,
                        event = connection.poll() => { let _ = event?; }
                    }
                }
            }
        }
        .await;
        eprintln!("MQTT ended: {result:?}; reconnecting in 5 seconds");
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}
