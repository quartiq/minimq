use minimq::{Minimq, Publication, QoS};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let mut rx_buffer = [0u8; 256];
    let mut tx_buffer = [0u8; 256];
    let stack = std_embedded_nal::Stack::default();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt = Minimq::<_, _, 256, 16>::new(
        localhost,
        "",
        stack,
        StandardClock::default(),
        &mut rx_buffer,
        &mut tx_buffer,
    )
    .unwrap();

    // Use a keepalive interval for the client.
    mqtt.client().set_keepalive_interval(60).unwrap();

    let mut published = false;
    let mut subscribed = false;

    loop {
        // Service the MQTT client until there's no more data to process.
        while let Some(complete) = mqtt
            .poll(|_client, topic, _payload, _properties| topic == "data")
            .unwrap()
        {
            if complete {
                log::info!("Transmission complete");
                std::process::exit(0);
            }
        }

        if !mqtt.client().is_connected() {
            continue;
        }

        if !subscribed {
            mqtt.client().subscribe(&["data".into()], &[]).unwrap();
            subscribed = true;
        }

        if !mqtt.client().subscriptions_pending()
            && !published
            && mqtt.client().can_publish(QoS::ExactlyOnce)
        {
            mqtt.client()
                .publish(
                    Publication::new("Ping".as_bytes())
                        .topic("data")
                        .qos(QoS::ExactlyOnce)
                        .finish()
                        .unwrap(),
                )
                .unwrap();
            log::info!("Publishing message");
            published = true;
        }
    }
}
