mod support;

use embedded_io_async::Error as _;
use embedded_io_async::ErrorKind;
use minimq::{
    Broker, Buffers, ConfigBuilder, Error, Event, Publication, QoS, Session,
    transport::{AddrType, DnsTcpConnector, TcpConnector},
    types::{SubscriptionOptions, TopicFilter},
};
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};
use std_embedded_nal_async::Stack as StdStack;
use support::block_on;

const BROKER_ADDR_ENV: &str = "MINIMQ_REAL_BROKER_ADDR";
const BROKER_HOST_ENV: &str = "MINIMQ_REAL_BROKER_HOST";

fn socket_broker() -> Option<SocketAddr> {
    let raw = std::env::var(BROKER_ADDR_ENV).ok()?;
    Some(
        raw.parse()
            .unwrap_or_else(|_| panic!("invalid {BROKER_ADDR_ENV} value: {raw}")),
    )
}

fn hostname_broker() -> Option<String> {
    std::env::var(BROKER_HOST_ENV).ok()
}

fn unique_client_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq-{label}-{nanos}")
}

fn unique_topic() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("minimq/test/{nanos}")
}

fn config<'a>(broker: Broker<'a>, client_id: &str) -> ConfigBuilder<'a> {
    let rx = Box::leak(Box::new([0; 1024]));
    let tx = Box::leak(Box::new([0; 2048]));
    ConfigBuilder::new(broker, Buffers::new(rx, tx))
        .client_id(client_id)
        .unwrap()
}

fn poll_until_ready<'a, C>(
    session: &mut Session<'_, 'a, C>,
    want_inbound: bool,
) -> Option<(String, Vec<u8>, QoS)>
where
    C: minimq::transport::Connector,
    C::Error: embedded_io_async::Error + core::fmt::Debug,
{
    for _ in 0..200 {
        match block_on(session.poll()) {
            Ok(Event::Idle | Event::Connected | Event::Reconnected) if !want_inbound => {
                return None;
            }
            Ok(Event::Inbound(message)) if want_inbound => {
                return Some((
                    message.topic().to_string(),
                    message.payload().to_vec(),
                    message.qos(),
                ));
            }
            Ok(_) => {}
            Err(Error::Transport(err)) => match err.kind() {
                ErrorKind::TimedOut | ErrorKind::Interrupted => {}
                _ => panic!("session poll failed: {err:?}"),
            },
            Err(err) => panic!("session poll failed: {err:?}"),
        }
    }
    panic!("timed out waiting for broker activity");
}

fn assert_roundtrip<'a, C>(
    subscriber: &mut Session<'_, 'a, C>,
    publisher: &mut Session<'_, 'a, C>,
    topic: &str,
    payload: &[u8],
) where
    C: minimq::transport::Connector,
    C::Error: embedded_io_async::Error + core::fmt::Debug,
{
    match block_on(subscriber.poll()).unwrap() {
        Event::Connected => {}
        other => panic!("unexpected event: {other:?}"),
    }
    let topics = [TopicFilter::new(topic)
        .options(SubscriptionOptions::default().maximum_qos(QoS::AtLeastOnce))];
    block_on(subscriber.subscribe(&topics, &[])).unwrap();
    let _ = poll_until_ready(subscriber, false);

    match block_on(publisher.poll()).unwrap() {
        Event::Connected => {}
        other => panic!("unexpected event: {other:?}"),
    }
    block_on(publisher.publish(Publication::new(topic, payload).qos(QoS::AtLeastOnce))).unwrap();

    let (received_topic, received_payload, received_qos) =
        poll_until_ready(subscriber, true).expect("publish");
    assert_eq!(received_topic, topic);
    assert_eq!(received_payload, payload);
    assert_eq!(received_qos, QoS::AtLeastOnce);
}

#[test]
fn real_broker_qos1_roundtrip_over_tcp_connector() {
    let Some(addr) = socket_broker() else {
        eprintln!("skipping real broker test; set {BROKER_ADDR_ENV}=host:port");
        return;
    };

    let connector = TcpConnector::new(StdStack::default());
    let mut subscriber = Session::new(config(addr.into(), &unique_client_id("sub")), &connector);
    let mut publisher = Session::new(config(addr.into(), &unique_client_id("pub")), &connector);
    let topic = unique_topic();

    assert_roundtrip(
        &mut subscriber,
        &mut publisher,
        &topic,
        b"hello from minimq",
    );
}

#[test]
fn real_broker_qos1_roundtrip_over_dns_connector() {
    let Some(host) = hostname_broker() else {
        eprintln!("skipping hostname broker test; set {BROKER_HOST_ENV}=hostname");
        return;
    };

    let broker = Broker::hostname(
        &host,
        socket_broker().map(|addr| addr.port()).unwrap_or(1883),
    );
    let stack = StdStack::default();
    let connector = DnsTcpConnector::new(stack.clone(), stack, AddrType::IPv4);
    let mut subscriber = Session::new(config(broker, &unique_client_id("dns-sub")), &connector);
    let mut publisher = Session::new(config(broker, &unique_client_id("dns-pub")), &connector);
    let topic = unique_topic();

    assert_roundtrip(&mut subscriber, &mut publisher, &topic, b"hello over dns");
}
