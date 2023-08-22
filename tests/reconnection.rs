use minimq::{
    types::{SubscriptionOptions, TopicFilter},
    Minimq, Publication, QoS,
};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

mod stack;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let mut rx_buffer = [0u8; 256];
    let mut tx_buffer = [0u8; 256];
    let mut session = [0u8; 256];
    let sockets = std::cell::RefCell::new(Vec::new());
    let stack = stack::MitmStack::new(&sockets);
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
    mqtt.client().set_keepalive_interval(1).unwrap();

    // 1. Poll until we're connected and subscribed to a test topic
    while !mqtt.client().is_connected() {
        mqtt.poll(|_client, _topic, _payload, _properties| {})
            .unwrap();
    }

    let topic_filter = TopicFilter::new("test")
        .options(SubscriptionOptions::default().maximum_qos(QoS::ExactlyOnce));
    mqtt.client().subscribe(&[topic_filter], &[]).unwrap();

    while mqtt.client().subscriptions_pending() {
        mqtt.poll(|_client, _topic, _payload, _properties| {})
            .unwrap();
    }

    // 2. Send a QoS::AtLeastOnce message
    mqtt.client()
        .publish(
            Publication::new("Ping".as_bytes())
                .topic("test")
                .qos(QoS::ExactlyOnce)
                .finish()
                .unwrap(),
        )
        .unwrap();

    // Force a disconnect from the broker.
    for socket in sockets.borrow_mut().iter_mut() {
        socket.1.close();
    }

    // 3. Wait until the keepalive timeout lapses and we disconnect from the broker.
    while mqtt.client().is_connected() {
        mqtt.poll(|_client, _topic, _payload, _properties| {})
            .unwrap();
    }

    assert!(mqtt.client().pending_messages());

    // 4. Poll until we're reconnected
    while !mqtt.client().is_connected() {
        mqtt.poll(|_client, _topic, _payload, _properties| {})
            .unwrap();
    }

    // 5. Verify that we finish transmission of our pending message.
    let mut rx_messages = 0;
    while mqtt.client().pending_messages() || rx_messages == 0 {
        mqtt.poll(|_client, _topic, _payload, _properties| {
            rx_messages += 1;
        })
        .unwrap();
    }

    // 5. Verify that we receive the message after reconnection
    assert!(rx_messages == 1);

    Ok(())
}
