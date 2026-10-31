mod support;

use embedded_io_adapters::tokio_1::FromTokio;
use minimq::{
    Buffers, ConfigBuilder, ConnectEvent, Connection, Op, Publication, QoS, Session,
    SubscriptionOptions, TopicFilter,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use support::init_host_logging;
use tokio::{net::TcpStream, time::timeout};

type Conn<'a, 'buf> = Connection<'a, 'buf, FromTokio<TcpStream>>;

async fn complete(conn: &mut Conn<'_, '_>, op: Op) {
    while conn.is_pending(&op) {
        assert!(conn.poll().await.unwrap().is_none());
    }
    assert!(conn.is_complete(&op));
}

#[tokio::test]
#[ignore = "requires BROKER=host:port"]
async fn both_protocols_qos_will_and_sessions() {
    init_host_logging();
    let broker = std::env::var("BROKER").expect("set BROKER=host:port");
    timeout(Duration::from_secs(15), async {
        for protocol in [
            minimq::Protocol::V5,
            minimq::Protocol::V3_1_1 {
                clean_session: false,
            },
            minimq::Protocol::V3_1_1 {
                clean_session: true,
            },
        ] {
            let id = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let topic = format!("minimq/test/{id}");
            let mut subscriber_config = config(&format!("minimq-sub-{id}")).protocol(protocol);
            if protocol == minimq::Protocol::V5 {
                subscriber_config = subscriber_config.session_expiry_interval(60);
            }
            let mut subscriber = Session::new(subscriber_config);
            let mut publisher = Session::new(
                config(&format!("minimq-pub-{id}"))
                    .protocol(protocol)
                    .will(
                        minimq::Will::new(&topic, b"gone", &[])
                            .unwrap()
                            .qos(QoS::AtLeastOnce),
                    )
                    .unwrap(),
            );
            let mut sub = subscriber
                .connect(FromTokio::new(TcpStream::connect(&broker).await.unwrap()))
                .await
                .unwrap();
            assert_eq!(sub.connect_event(), ConnectEvent::Connected);
            let filters = [TopicFilter::new(&topic)
                .options(SubscriptionOptions::default().maximum_qos(QoS::ExactlyOnce))];
            let op = sub.subscribe(&filters, &[]).await.unwrap();
            complete(&mut sub, op).await;
            drop(sub);
            let mut sub = subscriber
                .connect(FromTokio::new(TcpStream::connect(&broker).await.unwrap()))
                .await
                .unwrap();
            if protocol
                == (minimq::Protocol::V3_1_1 {
                    clean_session: true,
                })
            {
                assert_eq!(sub.connect_event(), ConnectEvent::Connected);
                let op = sub.subscribe(&filters, &[]).await.unwrap();
                complete(&mut sub, op).await;
            } else {
                assert_eq!(sub.connect_event(), ConnectEvent::Reconnected);
            }
            let mut publisher = publisher
                .connect(FromTokio::new(TcpStream::connect(&broker).await.unwrap()))
                .await
                .unwrap();
            for qos in [QoS::AtMostOnce, QoS::AtLeastOnce, QoS::ExactlyOnce] {
                let op = publisher
                    .publish(Publication::bytes(&topic, b"hello").qos(qos))
                    .await
                    .unwrap();
                let (received, ()) = tokio::join!(sub.recv(), async {
                    if let Some(op) = op {
                        complete(&mut publisher, op).await;
                    }
                });
                let received = received.unwrap();
                assert_eq!(received.topic(), topic);
                assert_eq!(received.payload(), b"hello");
                assert_eq!(received.qos(), qos);
                assert!(sub.drive().await.unwrap().is_none());
            }
            drop(publisher); // Abrupt TCP close publishes the Will for either version.
            assert_eq!(sub.recv().await.unwrap().payload(), b"gone");
            let op = sub.unsubscribe(&[&topic], &[]).await.unwrap();
            complete(&mut sub, op).await;
            sub.disconnect().await.unwrap();
        }
    })
    .await
    .expect("broker exchange timed out");
}

fn config(client_id: &str) -> ConfigBuilder<'static> {
    let rx = Box::leak(Box::new([0; 1024]));
    let tx = Box::leak(Box::new([0; 2048]));
    ConfigBuilder::new(Buffers::new(rx, tx))
        .client_id(client_id)
        .unwrap()
}
