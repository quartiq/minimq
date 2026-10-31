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
async fn rejected_startup_waits_for_explicit_restart() {
    use tokio::io::AsyncWriteExt;
    let (client, mut broker) = tokio::io::duplex(1024);
    broker
        .write_all(&[0x20, 3, 0, 0, 0, 0x90, 4, 0, 1, 0, 0x87])
        .await
        .unwrap();
    let mut session = Session::new(config());
    let mut connection = session.connect(FromTokio::new(client)).await.unwrap();
    let mut service = Service::new("device").unwrap();
    service.begin_connection(connection.connect_event());
    assert!(!service.step(&mut connection).await.unwrap());
    service.begin_connection(minimq::ConnectEvent::SessionResumed);
    assert!(!service.step(&mut connection).await.unwrap());
    let minimq::Event::Completed { op, outcome } = connection.poll().await.unwrap() else {
        panic!("expected completion")
    };
    let rejection = minimq::PeerError::Rejected(minimq::ReasonCode::NotAuthorized);
    assert!(service.handle_completion(op, &outcome));
    for _ in 0..2 {
        assert!(
            matches!(service.step(&mut connection).await, Err(minimq::Error::Peer(error)) if error == rejection)
        );
        assert!(!service.is_ready());
    }
    service.begin_connection(minimq::ConnectEvent::SessionResumed);
    assert!(!service.step(&mut connection).await.unwrap());
}

#[tokio::test]
async fn omitted_completion_is_reported_instead_of_waiting() {
    use tokio::io::AsyncWriteExt;
    let (client, mut broker) = tokio::io::duplex(1024);
    broker
        .write_all(&[0x20, 3, 0, 0, 0, 0x90, 4, 0, 1, 0, 1])
        .await
        .unwrap();
    let mut session = Session::new(config());
    let mut connection = session.connect(FromTokio::new(client)).await.unwrap();
    let mut service = Service::new("device").unwrap();
    assert!(!service.step(&mut connection).await.unwrap());
    assert!(matches!(
        connection.poll().await.unwrap(),
        minimq::Event::Completed { .. }
    ));
    for _ in 0..2 {
        assert!(matches!(
            service.step(&mut connection).await,
            Err(minimq::Error::MissingCompletion)
        ));
        assert!(!service.is_ready());
    }
    connection.disconnect().await.unwrap();
    assert!(matches!(
        service.step(&mut connection).await,
        Err(minimq::Error::Disconnected)
    ));
}

#[tokio::test]
async fn rejects_mqtt_v3_connection() {
    init_logging();
    let Ok(addr) = std::env::var("BROKER") else {
        return;
    };
    let mut session = Session::new(config().protocol(minimq::Protocol::V3_1_1 {
        clean_session: true,
    }));
    let mut connection = session
        .connect(FromTokio::new(TcpStream::connect(addr).await.unwrap()))
        .await
        .unwrap();
    let mut service = Service::new("v3-rejection").unwrap();
    assert!(matches!(
        service.step(&mut connection).await,
        Err(minimq::Error::InvalidRequest)
    ));
    assert!(!service.is_ready());
    connection.disconnect().await.unwrap();
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
        if let minimq::Event::Completed { op, outcome } = device.poll().await.unwrap() {
            rpc.handle_completion(op, &outcome);
        }
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

    let minimq::Event::Publish(inbound) = timeout(Duration::from_secs(5), device.poll())
        .await
        .unwrap()
        .unwrap()
    else {
        panic!("expected publication")
    };
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

    let response = loop {
        match timeout(Duration::from_secs(5), client.poll())
            .await
            .unwrap()
            .unwrap()
        {
            minimq::Event::Publish(response) => break response,
            minimq::Event::Completed { outcome, .. } => outcome.check().unwrap(),
        }
    };
    assert_eq!(response.payload(), b"pong");
    assert_eq!(response.correlation_data(), Some(correlation.as_slice()));
    assert!(
        response
            .properties()
            .iter()
            .any(|property| matches!(property, Ok(Property::UserProperty("code", SUCCESS_CODE))))
    );
}
