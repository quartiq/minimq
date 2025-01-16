use minimq::{
    types::{SubscriptionOptions, TopicFilter},
    Minimq, Publication, QoS,
};

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
    let mut received_messages = 0;

    loop {
        // Continually process the client until no more data is received.
        while let Some(count) = mqtt
            .poll(|_client, _topic, _payload, _properties| 1)
            .unwrap()
        {
            log::info!("Received a message");
            received_messages += count;
        }

        if !mqtt.client().is_connected() {
            continue;
        }

        if !subscribed {
            let topic_filter = TopicFilter::new("data")
                .options(SubscriptionOptions::default().maximum_qos(QoS::ExactlyOnce));
            mqtt.client().subscribe(&[topic_filter], &[]).unwrap();
            subscribed = true;
        }

        if mqtt.client().subscriptions_pending() {
            continue;
        }

        if !published && mqtt.client().can_publish(QoS::ExactlyOnce) {
            mqtt.client()
                .publish(Publication::new("data", b"Ping").qos(QoS::ExactlyOnce))
                .unwrap();
            log::info!("Publishing message");
            published = true;
        }

        if received_messages > 0 && !mqtt.client().pending_messages() {
            assert!(published);
            log::info!("Reception Complete");
            std::process::exit(0);
        }
    }
}
