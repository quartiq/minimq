use minimq::{DeferredPublication, Minimq, QoS};

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

    while !mqtt.client().is_connected() {
        mqtt.poll(|_client, _topic, _payload, _properties| {})
            .unwrap();
    }

    assert!(matches!(
        mqtt.client().publish(
            DeferredPublication::new("data", |_buf| { Err("Oops!") }).qos(QoS::ExactlyOnce)
        ),
        Err(minimq::PubError::Serialization("Oops!"))
    ));

    Ok(())
}
