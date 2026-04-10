[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

`minimq` is a small `no_std` MQTT v5 client for embedded systems.

It is built for applications that want:

- one long-lived MQTT session object
- explicit caller-owned buffers
- async transport over [`embedded_io_async`]
- MQTT request/reply support without extra glue

The main API is [`Session`].

## What It Gives You

- MQTT v5 publish and subscribe
- QoS 0, 1, and 2 for outgoing publishes
- reconnect and session resumption
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
        BufferLayout { rx: 256, outbound: 768 },
    )?
    .client_id("demo")?
    .build();

    let connector = Stack::default();
    let mut session = Session::new(config, &connector);

    loop {
        match session.poll().await? {
            Event::Connected | Event::Reconnected => {
                session.subscribe(&[TopicFilter::new("demo/in")], &[]).await?;
            }
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
2. three byte buffers
3. a transport connector
4. a loop that keeps calling `poll()`

The session then owns reconnects, keepalive, packet flow, and inbound message delivery.

Core types:

- [`Broker`]: broker endpoint config
- [`Buffers`]: caller-owned RX/outbound memory
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

## Buffers

`minimq` keeps memory explicit.

You supply two buffers:

- `rx`: inbound packet storage
- `outbound`: outbound packet encoding plus retransmission storage for QoS/session handling

The outbound buffer is shared:

- QoS 1 and 2 publishes are retained there until the broker acknowledges them
- the same bytes are reused to replay those publishes after a reconnect
- QoS 0 publishes and larger control packets also encode there transiently

That means outbound capacity is not just "how big a single publish may be". It also bounds how much
unacknowledged QoS traffic the session can retain. If that arena is full, `publish()` can return
[`Error::NotReady`] until
the broker advances the in-flight state.

If you prefer one contiguous slab, use
[`BufferLayout::split()`]
to carve it into named regions.

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
- [`embassy_time`] for timing

For RTIC or any non-Embassy executor, the final binary crate must enable an
`embassy-time` `generic-queue-*` feature. `minimq` does not choose the queue feature for you.

If you use `embassy-stm32`, the final binary also needs an `embassy-stm32` `time-driver-*`
feature.

## Current Shape

The crate is intentionally narrow:

- one preferred session API
- no allocator
- no sync transport compatibility layer
- no attempt to abstract over every possible runtime or socket model

That keeps the dominant embedded async use case simple.

## Repository Tests

The repository contains:

- deterministic protocol tests in
  `tests/async_client.rs`
- optional live-broker smoke tests in
  `tests/real_broker.rs`

The real-broker tests are only for integration coverage. They are skipped unless broker
environment variables are provided.
