use minimq::{Minimq, QoS, Retain};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let stack = std_embedded_nal::Stack::default();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt =
        Minimq::<_, _, 256, 16>::new(localhost, "", stack, StandardClock::default()).unwrap();

    // Use a keepalive interval for the client.
    mqtt.client().set_keepalive_interval(60).unwrap();

    let mut published = false;
    let mut subscribed = false;

    loop {
        // Service the MQTT client until there's no more data to process.
        loop {
            match mqtt
                .poll(|_client, topic, _payload, _properties| topic == "data")
                .unwrap()
            {
                Some(true) => {
                    log::info!("Transmission complete");
                    std::process::exit(0);
                }
                Some(_) => {}
                None => break,
            }
        }

        if !mqtt.client().is_connected() {
            continue;
        }

        if !subscribed {
            mqtt.client().subscribe("data", &[]).unwrap();
            subscribed = true;
        }

        if !mqtt.client().subscriptions_pending()
            && !published
            && mqtt.client().can_publish(QoS::ExactlyOnce)
        {
            mqtt.client()
                .publish(
                    "data",
                    "Ping".as_bytes(),
                    QoS::ExactlyOnce,
                    Retain::NotRetained,
                    &[],
                )
                .unwrap();
            log::info!("Publishing message");
            published = true;
        }
    }
}
