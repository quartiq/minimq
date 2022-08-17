use minimq::{Minimq, Property, QoS, Retain};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();
    log::info!("Starting test");

    let stack = std_embedded_nal::Stack::default();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt =
        Minimq::<_, _, 256, 16>::new(localhost, "", stack, StandardClock::default()).unwrap();

    // Use a keepalive interval for the client.
    mqtt.client().set_keepalive_interval(60).unwrap();

    let mut published = false;
    let mut subscribed = false;
    let mut responses = 0;

    mqtt.client()
        .set_will(
            "exit",
            "Test complete".as_bytes(),
            QoS::AtMostOnce,
            Retain::NotRetained,
            &[],
        )
        .unwrap();

    loop {
        mqtt.poll(|client, topic, payload, properties| {
            println!("{} < {}", topic, core::str::from_utf8(payload).unwrap());

            for property in properties {
                if let Property::ResponseTopic(topic) = property {
                    client
                        .publish(
                            topic,
                            "Pong".as_bytes(),
                            QoS::AtMostOnce,
                            Retain::NotRetained,
                            &[],
                        )
                        .unwrap();
                }
            }

            if topic == "response" {
                responses += 1;
                if responses == 2 {
                    assert_eq!(0, client.pending_messages(QoS::AtLeastOnce));
                    std::process::exit(0);
                }
            }
        })
        .unwrap();
        let client = mqtt.client();

        if !subscribed {
            if client.is_connected() {
                client.subscribe("response", &[]).unwrap();
                client.subscribe("request", &[]).unwrap();
                subscribed = true;
            }
        } else if !client.subscriptions_pending() && !published {
            println!("PUBLISH request");
            let properties = [Property::ResponseTopic("response")];
            client
                .publish(
                    "request",
                    "Ping".as_bytes(),
                    QoS::AtMostOnce,
                    Retain::NotRetained,
                    &properties,
                )
                .unwrap();

            client
                .publish(
                    "request",
                    "Ping".as_bytes(),
                    QoS::AtLeastOnce,
                    Retain::NotRetained,
                    &properties,
                )
                .unwrap();

            // The message cannot be ack'd until the next poll call
            assert_eq!(1, client.pending_messages(QoS::AtLeastOnce));

            published = true;
        }
    }
}
