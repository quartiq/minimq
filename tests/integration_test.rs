use minimq::{types::Utf8String, Minimq, Property, Publication, QoS, Will};

use embedded_nal::{self, IpAddr, Ipv4Addr};
use std_embedded_time::StandardClock;

#[test]
fn main() -> std::io::Result<()> {
    env_logger::init();

    let will = Will::new("exit", "Test complete".as_bytes(), &[]).unwrap();

    let mut buffer = [0u8; 1024];
    let stack = std_embedded_nal::Stack;
    let localhost = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let mut mqtt: Minimq<'_, _, _, minimq::broker::IpBroker> = Minimq::new(
        stack,
        StandardClock::default(),
        minimq::ConfigBuilder::new(localhost.into(), &mut buffer)
            .will(will)
            .unwrap()
            .keepalive_interval(60),
    );

    let mut published = false;
    let mut subscribed = false;
    let mut responses = 0;

    loop {
        // Continually poll the client until there is no more data.
        while let Some(was_response) = mqtt
            .poll(|client, topic, payload, properties| {
                log::info!("{} < {}", topic, core::str::from_utf8(payload).unwrap());

                if let Ok(response) = Publication::new("Pong".as_bytes())
                    .reply(properties)
                    .finish()
                {
                    client.publish(response).unwrap();
                }

                topic == "response"
            })
            .unwrap()
        {
            if was_response {
                responses += 1;
                if responses == 2 {
                    assert!(!mqtt.client().pending_messages());
                    std::process::exit(0);
                }
            }
        }

        let client = mqtt.client();

        if !subscribed {
            if client.is_connected() {
                client
                    .subscribe(&["response".into(), "request".into()], &[])
                    .unwrap();
                subscribed = true;
            }
        } else if !client.subscriptions_pending() && !published {
            println!("PUBLISH request");
            let properties = [Property::ResponseTopic(Utf8String("response"))];
            let publication = Publication::new(b"Ping")
                .topic("request")
                .properties(&properties)
                .finish()
                .unwrap();

            client.publish(publication).unwrap();

            let publication = Publication::new(b"Ping")
                .topic("request")
                .properties(&properties)
                .qos(QoS::AtLeastOnce)
                .finish()
                .unwrap();
            client.publish(publication).unwrap();

            // The message cannot be ack'd until the next poll call
            assert!(client.pending_messages());

            published = true;
        }
    }
}
