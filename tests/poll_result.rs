use minimq::{Minimq, Publication, QoS};

use core::net::{IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let mut buffer = [0u8; 1024];
    let stack = std_embedded_nal::Stack;
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt: Minimq<'_, _, _, minimq::broker::IpBroker> = Minimq::new(
        stack,
        StandardClock::default(),
        minimq::ConfigBuilder::new(localhost.into(), &mut buffer).keepalive_interval(60),
    );

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
                .publish(Publication::new("data", b"Ping").qos(QoS::ExactlyOnce))
                .unwrap();
            log::info!("Publishing message");
            published = true;
        }
    }
}
