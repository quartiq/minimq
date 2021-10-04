use minimq::{Minimq, Property, QoS};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use embedded_time::{fraction::Fraction, Clock};
use std::time::Instant;

#[derive(Default)]
struct StdClock {
    start: core::cell::UnsafeCell<Option<Instant>>,
}

impl Clock for StdClock {
    type T = u32;

    const SCALING_FACTOR: Fraction = Fraction::new(1, 1_000);

    fn try_now(&self) -> Result<embedded_time::Instant<Self>, embedded_time::clock::Error> {
        let std_now = Instant::now();
        let start = unsafe {
            if (*self.start.get()).is_none() {
                (*self.start.get()).replace(std_now);
                std_now
            } else {
                (*self.start.get()).unwrap()
            }
        };

        let elapsed = std_now - start;

        Ok(embedded_time::Instant::new(elapsed.as_millis() as u32))
    }
}

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let stack = std_embedded_nal::Stack::default();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt = Minimq::<_, _, 256>::new(localhost, "", stack, StdClock::default()).unwrap();

    let mut published = false;
    let mut subscribed = false;

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
                std::process::exit(0);
            }
        })
        .unwrap();

        if !subscribed {
            if mqtt.client.is_connected() {
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

                    published = true;
                }
            }
        }
    }
}
