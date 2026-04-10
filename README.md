[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

`minimq` is a small `no_std`, no-alloc, `async` MQTT v5 client for embedded systems.

It is built for applications that want:

- one long-lived MQTT session object with automatic reconnect
- explicit caller-owned RX/TX packet buffers
- async transport over [`embedded_io_async`]
- MQTT5 request/reply support without extra glue

The main API is [`Session`].

## What It Gives You

- MQTT v5 publish and subscribe
- QoS 0, 1, and 2 for outgoing publishes
- will, reconnect and session resumption
- anonymous and plain text authentication
- retained publishes and will messages
- explicit connection lifecycle events
- zero-copy inbound payload access
- no allocator requirement

`minimq` is a good fit when you already have an async TCP stack and want a focused MQTT client,
not a whole application framework.

## Example

```ignore
use core::net::SocketAddr;
use minimq::{
    Broker, BufferLayout, ConfigBuilder, Event, QoS, Session, types::TopicFilter,
};
use std_embedded_nal_async::Stack;

async fn run() -> Result<(), minimq::Error> {
    let mut storage = [0u8; 1024];
    let broker: Broker<'_> = "127.0.0.1:1883".parse::<SocketAddr>()?.into();
    let config = ConfigBuilder::from_buffer_layout(
        broker,
        &mut storage,
        BufferLayout { rx: 256, tx: 768 },
    )?
    .client_id("demo")?
    .build();

    let connector = Stack::default();
    let mut session = Session::new(config, &connector);

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

## What Using It Looks Like

You provide:

1. a broker endpoint
2. byte buffers
3. a transport connector
4. a loop that keeps calling `poll()`

The session then owns reconnects, keepalive, packet flow, and inbound message delivery.

Core types:

- [`Broker`]: broker endpoint config
- [`Buffers`]: caller-owned RX/TX memory
- [`ConfigBuilder`]: session configuration
- [`Session`]: the MQTT client you drive
- [`Event`]: what `poll()` produced

Typical flow:

- build a `ConfigBuilder`
- construct a `Session`
- call `poll()` regularly
- react to:
  - `Event::Connected`
  - `Event::Reconnected`
  - `Event::Inbound(message)`
  - `Event::Idle`
- call `publish()` and `subscribe()` on the same session

`Event::Connected` means you have a fresh broker session and should establish any subscriptions or
other session state. `Event::Reconnected` means the broker resumed the existing session, so
subscriptions and in-flight QoS state were kept.

## Buffers

`minimq` keeps memory explicit.

You supply two buffers:

- `rx`: storage for one inbound MQTT packet. Size it for the largest packet you expect to receive.
- `tx`: outbound encode and replay storage. It must cover the largest outbound packet and any
  retained in-flight QoS/session state.

If `tx` is full, `publish()` can return [`Error::NotReady`] until the broker advances the in-flight
state.

Use [`BufferLayout::split()`] if you prefer one contiguous slab.

## Request / Reply

`minimq` understands MQTT request/reply properties directly.

On inbound publishes, [`InboundPublish`]
can:

- inspect `ResponseTopic`
- inspect `CorrelationData`
- build a direct reply with `reply(...)`
- capture an owned reply target with `reply_owned(...)`

That is useful for protocol layers such as settings, RPC, or telemetry control built on top of
MQTT.

## Transport And Time

`minimq` uses:

- [`embedded_io_async`] for byte I/O
- [`embedded_nal_async`] adapters in `transport`
- [`embassy_time`] for timing. It does not choose a queue feature for you.

Secure MQTT over TLS works fine, for example with embedded-tls: `examples/tls_public_broker.rs`.
