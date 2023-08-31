use minimq::{DeferredPublication, Minimq, QoS};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let mut rx_buffer = [0u8; 256];
    let mut tx_buffer = [0u8; 256];
    let mut session = [0u8; 256];
    let stack = std_embedded_nal::Stack;
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt: Minimq<'_, _, _, minimq::broker::IpBroker> = Minimq::new(
        stack,
        StandardClock::default(),
        minimq::Config::new(localhost.into(), &mut rx_buffer, &mut tx_buffer)
            .session_state(&mut session)
            .keepalive_interval(60),
    );

    while !mqtt.client().is_connected() {
        mqtt.poll(|_client, _topic, _payload, _properties| {})
            .unwrap();
    }

    assert!(matches!(
        mqtt.client().publish(
            DeferredPublication::new(|_buf| { Err("Oops!") })
                .topic("data")
                .qos(QoS::ExactlyOnce)
                .finish()
                .unwrap(),
        ),
        Err(minimq::PubError::Serialization("Oops!"))
    ));

    Ok(())
}
