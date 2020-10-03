use minimq::{consts, MqttClient, Property, QoS};

use embedded_nal::{self, IpAddr, Ipv4Addr};

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let stack = std_embedded_nal::STACK.clone();
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut client =
        MqttClient::<consts::U256, _>::new(localhost, "IntegrationTest", stack).unwrap();

    let mut published = false;
    let mut subscribed = false;

    loop {
        client
            .poll(|client, topic, payload, properties| {
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
            if client.is_connected().unwrap() {
                client.subscribe("response", &[]).unwrap();
                client.subscribe("request", &[]).unwrap();
                subscribed = true;
            }
        } else {
            if client.subscriptions_pending() == false {
                if !published {
                    println!("PUBLISH request");
                    let properties = [Property::ResponseTopic("response")];
                    client
                        .publish("request", "Ping".as_bytes(), QoS::AtMostOnce, &properties)
                        .unwrap();

                    published = true;
                }
            }
        }
    }
}
