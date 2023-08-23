use minimq::{
    types::{SubscriptionOptions, TopicFilter},
    Minimq, Publication, QoS,
};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let mut rx_buffer = [0u8; 256];
    let mut tx_buffer = [0u8; 256];
    let mut session = [0u8; 256];
    let stack = std_embedded_nal::Stack::default();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt: Minimq<'_, _, _, minimq::broker::IpBroker> = Minimq::new(
        localhost.into(),
        "",
        stack,
        StandardClock::default(),
        &mut rx_buffer,
        &mut tx_buffer,
        &mut session,
    )
    .unwrap();

    // Use a keepalive interval for the client.
    mqtt.client().set_keepalive_interval(60).unwrap();

    let mut published = false;
    let mut subscribed = false;
    let mut received_messages = 0;

    loop {
        // Continually process the client until no more data is received.
        while let Some(count) = mqtt
            .poll(|_client, _topic, _payload, _properties| 1)
            .unwrap()
        {
            received_messages += count;
        }

        if !mqtt.client().is_connected() {
            continue;
        }

        if !subscribed {
            let topic_filter = TopicFilter::new("data")
                .options(SubscriptionOptions::default().maximum_qos(QoS::AtLeastOnce));
            mqtt.client().subscribe(&[topic_filter], &[]).unwrap();
            subscribed = true;
        }

        if mqtt.client().subscriptions_pending() {
            continue;
        }

        if !published && mqtt.client().can_publish(QoS::AtLeastOnce) {
            mqtt.client()
                .publish(
                    Publication::new("Ping".as_bytes())
                        .topic("data")
                        .qos(QoS::AtLeastOnce)
                        .finish()
                        .unwrap(),
                )
                .unwrap();
            log::info!("Publishing message");
            published = true;
        }

        if received_messages > 0 {
            assert!(published);
            assert!(!mqtt.client().pending_messages());
            log::info!("Reception Complete");
            std::process::exit(0);
        }
    }
}
