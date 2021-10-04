use minimq::{Minimq, Property, QoS};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let stack = std_embedded_nal::Stack::default();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt =
        Minimq::<_, _, 256>::new(localhost, "", stack, StandardClock::default()).unwrap();

    let mut published = false;
    let mut subscribed = false;
    let mut responses = 0;

    loop {
        mqtt.poll(|client, topic, payload, properties| {
            println!("{} < {}", topic, core::str::from_utf8(payload).unwrap());

            for property in properties {
                match property {
                    Property::ResponseTopic(topic) => client
                        .publish(topic, "Pong".as_bytes(), QoS::AtMostOnce, &[])
                        .unwrap(),
                    _ => {}
                };
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

        if !subscribed {
            if mqtt.client.is_connected().unwrap() {
                mqtt.client.subscribe("response", &[]).unwrap();
                mqtt.client.subscribe("request", &[]).unwrap();
                subscribed = true;
            }
        } else {
            if mqtt.client.subscriptions_pending() == false {
                if !published {
                    println!("PUBLISH request");
                    let properties = [Property::ResponseTopic("response")];
                    mqtt.client
                        .publish("request", "Ping".as_bytes(), QoS::AtMostOnce, &properties)
                        .unwrap();

                    mqtt.client
                        .publish("request", "Ping".as_bytes(), QoS::AtLeastOnce, &properties)
                        .unwrap();

                    // The message cannot be ack'd until the next poll call
                    assert_eq!(1, mqtt.client.pending_messages(QoS::AtLeastOnce));

                    published = true;
                }
            }
        }
    }
}
