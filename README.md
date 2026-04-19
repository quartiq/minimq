[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

`minimq` is a small `no_std`, no-alloc, `async` MQTT v5 client for embedded systems.

Use it when your application already has async network I/O and needs one long-lived MQTT session
with explicit buffers and reconnect handling.

The main API is [`Session`].

## What You Use

- [`Broker`]: broker endpoint
- [`Buffers`]: caller-owned RX/TX memory
- [`ConfigBuilder`]: session configuration
- [`transport::Connector`]: transport boundary
- [`Session`]: the client you drive
- [`Event`]: output of [`Session::poll()`]

## Example

```ignore
use core::net::SocketAddr;
use minimq::{
    Broker, ConfigBuilder, Event, QoS, Session,
    transport::TcpConnector, types::TopicFilter,
};
use std_embedded_nal_async::Stack;

async fn run() -> Result<(), minimq::SessionError<TcpConnector<Stack>>> {
    let mut storage = [0u8; 1024];
    let broker: Broker<'_> = "127.0.0.1:1883".parse::<SocketAddr>()?.into();
    let connector = TcpConnector::new(Stack::default());
    let mut session = Session::new(
        ConfigBuilder::from_buffer(broker, &mut storage, 256)?.client_id("demo")?,
        &connector,
    );

    loop {
        match session.poll().await? {
            Event::Connected => {
                session.subscribe(&[TopicFilter::new("demo/in")], &[]).await?;
            }
            Event::Reconnected => {}
            Event::Inbound(message) => {
                if let Some(reply) = message.reply("ack") {
                    session.publish(reply.qos(QoS::AtLeastOnce)).await?;
                }
            }
            Event::Idle => {}
        }
    }
}
```

## Session Model

You provide a broker endpoint, packet buffers, a transport connector, and a loop that keeps
calling [`Session::poll()`].

`Session` owns reconnects, keepalive, retransmission, and inbound packet delivery.

- [`Event::Connected`] means the broker created a fresh session. Re-establish subscriptions here.
- [`Event::Reconnected`] means the broker resumed the existing MQTT session. Existing
  subscriptions and in-flight QoS state were kept.
- [`Event::Inbound`] carries one inbound publish.
- [`Event::Idle`] means no inbound publish was produced on that poll step.

## Buffers

You supply two buffers.

- `rx` stores one inbound MQTT packet at a time. Size it for the largest inbound publish,
  including topic, properties, and payload.
- `tx` stores outbound encodes and retained in-flight state. Size it for the largest outbound
  packet plus the QoS/session state you want to keep active.

If `tx` is exhausted, `publish()` and other outbound operations can return [`Error::NotReady`].

Use [`Buffers::split()`] if you prefer one contiguous slab.

## Request / Reply

[`InboundPublish`] exposes MQTT v5 request/reply properties directly.

- [`InboundPublish::response_topic()`]
- [`InboundPublish::correlation_data()`]
- [`InboundPublish::reply()`]
- [`InboundPublish::reply_owned()`]

## Transport And Time

`minimq` uses:

- [`embedded_io_async`] for byte I/O
- [`embedded_nal_async`] adapters in `transport`
- [`embassy_time`] for timing

Secure MQTT over TLS works fine, for example with embedded-tls: `examples/tls_public_broker.rs`.
