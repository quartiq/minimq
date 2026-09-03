use embedded_io_adapters::tokio_1::FromTokio;
use minimq::{
    ConfigBuilder, Property, Publication, QoS, Session, SubscriptionOptions, TopicFilter,
};
use mqtt_rpc::{Handle, SUCCESS_CODE, Service, respond};
use std::{net::SocketAddr, sync::OnceLock};
use tokio::{
    net::TcpStream,
    time::{Duration, timeout},
};

fn init_logging() {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        env_logger::builder().is_test(true).try_init().unwrap();
        #[cfg(feature = "defmt")]
        defmt2log::init_from_current_exe();
    });
}

fn config() -> ConfigBuilder<'static> {
    ConfigBuilder::from_buffer(Box::leak(Box::new([0; 2048])), 1024).unwrap()
}

#[tokio::test]
async fn request_response() {
    init_logging();
    let Some(addr) = std::env::var("BROKER")
        .ok()
        .map(|addr| addr.parse::<SocketAddr>().unwrap())
    else {
        eprintln!("skipping broker test; set BROKER=host:port");
        return;
    };

    let io = FromTokio::new(TcpStream::connect(addr).await.unwrap());
    let mut device_session = Session::new(config());
    let mut device = timeout(Duration::from_secs(5), device_session.connect(io))
        .await
        .unwrap()
        .unwrap();
    let prefix = format!("mqtt-rpc-test-{}", std::process::id());
    let mut rpc = Service::new(&prefix).unwrap();
    rpc.begin_connection(device.connect_event());
    while !rpc.step(&mut device).await.unwrap() {
        let _ = device.poll().await.unwrap();
    }

    let io = FromTokio::new(TcpStream::connect(addr).await.unwrap());
    let mut client_session = Session::new(config());
    let mut client = timeout(Duration::from_secs(5), client_session.connect(io))
        .await
        .unwrap()
        .unwrap();
    let response_topic = format!("{prefix}/response");
    let subscription = client
        .subscribe(
            &[TopicFilter::new(&response_topic)
                .options(SubscriptionOptions::default().maximum_qos(QoS::AtLeastOnce))],
            &[],
        )
        .await
        .unwrap();
    while client.is_pending(&subscription) {
        let _ = client.poll().await.unwrap();
    }

    let correlation = b"test";
    let properties = [
        Property::ResponseTopic(&response_topic),
        Property::CorrelationData(correlation),
    ];
    client
        .publish(
            Publication::bytes(&format!("{prefix}/rpc/ping"), b"ping")
                .properties(&properties)
                .qos(QoS::AtLeastOnce),
        )
        .await
        .unwrap();

    let inbound = timeout(Duration::from_secs(5), device.recv())
        .await
        .unwrap()
        .unwrap();
    let Handle::Request(request) = rpc.handle(&inbound) else {
        panic!("request was not routed");
    };
    assert_eq!(
        (request.method(), request.payload()),
        ("ping", b"ping".as_slice())
    );
    let target = request.into_response_target();
    respond(&mut device, &target, SUCCESS_CODE, b"pong".as_slice())
        .await
        .unwrap();

    let response = timeout(Duration::from_secs(5), client.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.payload(), b"pong");
    assert_eq!(response.correlation_data(), Some(correlation.as_slice()));
    assert!(
        response
            .properties()
            .iter()
            .any(|property| matches!(property, Ok(Property::UserProperty("code", SUCCESS_CODE))))
    );
}
