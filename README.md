[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

`minimq` is a small `no_std`, no-alloc, `async` MQTT v5 client for embedded systems.

Use it when your application already has async network I/O and needs one long-lived MQTT session
with explicit buffers and reconnect handling.

The main API is [`Session`].

## What You Use

- [`Buffers`]: caller-owned RX/TX memory
- [`ConfigBuilder`]: session configuration
- [`Io`]: transport boundary for an established byte stream
- [`Session`]: the client you drive
- [`Event`]: output of [`Session::poll()`]

## Example

```ignore
use core::net::SocketAddr;
use minimq::{
    Buffers, ConfigBuilder, ConnectEvent, Error, Event, Io, QoS, Session, types::TopicFilter,
};
use tokio::net::TcpStream;

async fn open_io(addr: SocketAddr) -> Result<impl Io<Error = std::io::Error>, std::io::Error> {
    Ok(embedded_io_adapters::tokio_1::FromTokio::new(TcpStream::connect(addr).await?))
}

async fn run() -> Result<(), minimq::SessionError<impl Io<Error = std::io::Error>>> {
    let rx = &mut [0u8; 256];
    let tx = &mut [0u8; 768];
    let addr: SocketAddr = "127.0.0.1:1883".parse()?;
    let mut session = Session::new(ConfigBuilder::new(Buffers::new(rx, tx)).client_id("demo")?);

    loop {
        let io = open_io(addr).await?;
        match session.connect(io).await? {
            ConnectEvent::Connected => {
                session.subscribe(&[TopicFilter::new("demo/in")], &[]).await?;
            }
            ConnectEvent::Reconnected => {}
        }

        loop {
            match session.poll().await {
                Ok(Event::Inbound(message)) => {
                    if let Some(reply) = message.reply("ack") {
                        session.publish(reply.qos(QoS::AtLeastOnce)).await?;
                    }
                }
                Ok(Event::Idle) => {}
                Err(Error::Disconnected) => break,
                Err(err) => return Err(err),
            }
        }
    }
}
```

The attached transport must implement [`embedded_io_async::Read`], [`embedded_io_async::Write`],
[`embedded_io_async::ReadReady`], and [`embedded_io_async::WriteReady`].

For a TLS connectivity example that uses `embedded-tls` without those readiness traits, see
`examples/tls_public_broker.rs`. That example is useful for transport integration, but its
`poll()` loop is not bounded because `embedded-tls` does not expose read/write readiness.

## Session Model

You provide packet buffers plus an already-established transport, and a loop that explicitly
passes that transport into [`Session::connect()`] to establish or resume the broker session.

[`Session::connect()`] takes ownership of the provided transport and performs the unbounded MQTT
`CONNECT` / `CONNACK` handshake. Once
connected, [`Session::poll()`] does bounded keepalive, retransmission, and inbound packet delivery
on the established session. The session drops the transport again on graceful disconnect,
connection failure, or transport/protocol loss.

- [`ConnectEvent::Connected`] means the broker created a fresh session. Re-establish subscriptions
  here.
- [`ConnectEvent::Reconnected`] means the broker resumed the existing MQTT session. Existing
  subscriptions and in-flight QoS state were kept.
- [`Event::Inbound`] carries one inbound publish.
- [`Event::Idle`] means no inbound publish was produced on that bounded poll step.

If [`Session::poll()`] returns [`Error::Disconnected`], the caller decides when to call
[`Session::connect()`] with a fresh transport again.

## Buffers

You supply two buffers.

- `rx` stores one inbound MQTT packet at a time. Size it for the largest inbound publish,
  including topic, properties, and payload.
- `tx` stores outbound encodes and retained in-flight state. Size it for the largest outbound
  packet plus the QoS/session state you want to keep active.

If `tx` is exhausted, `publish()` and other outbound operations can return [`Error::NotReady`].
Malformed broker varints and undersized local encode buffers are rejected with errors rather than
causing panics.

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
- [`embassy_time`] for timing
