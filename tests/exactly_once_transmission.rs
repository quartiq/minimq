use minimq::{Minimq, Publication, QoS};

use embedded_nal::{self, IpAddr, Ipv4Addr};
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

    loop {
        // Continually process the client until no more data is received.
        while mqtt
            .poll(|_client, _topic, _payload, _properties| {})
            .unwrap()
            .is_some()
        {}

        if mqtt.client().is_connected() && !published && mqtt.client().can_publish(QoS::ExactlyOnce)
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

        if published && !mqtt.client().pending_messages() {
            log::info!("Transmission complete");
            std::process::exit(0);
        }
    }
}
