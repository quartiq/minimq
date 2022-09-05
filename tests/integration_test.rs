use minimq::{types::Utf8String, Minimq, Property, QoS, Retain};

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
        // Continually poll the client until there is no more data.
        while let Some(was_response) = mqtt
            .poll(|client, topic, payload, properties| {
                log::info!("{} < {}", topic, core::str::from_utf8(payload).unwrap());

                if let Some(response_topic) = properties.iter().find_map(|p| {
                    if let Property::ResponseTopic(topic) = p {
                        Some(topic)
                    } else {
                        None
                    }
                }) {
                    client
                        .publish(
                            response_topic.0,
                            "Pong".as_bytes(),
                            QoS::AtMostOnce,
                            Retain::NotRetained,
                            &[],
                        )
                        .unwrap();
                }

                topic == "response"
            })
            .unwrap()
        {
            if was_response {
                responses += 1;
                if responses == 2 {
                    assert_eq!(0, mqtt.client().pending_messages(QoS::AtLeastOnce));
                    std::process::exit(0);
                }
            }
        }

        let client = mqtt.client();

        if !subscribed {
            if client.is_connected() {
                client.subscribe(&["response".into(), "request".into()], &[]).unwrap();
                subscribed = true;
            }
        } else if !client.subscriptions_pending() && !published {
            println!("PUBLISH request");
            let properties = [Property::ResponseTopic(Utf8String("response"))];
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
