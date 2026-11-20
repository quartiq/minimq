[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

`minimq` is an opinionated `no_std`, no-alloc, `async` MQTT 5 and MQTT 3.1.1 client for embedded systems.

It manages one broker session over caller-provided packet buffers and an
[`embedded_io_async`] byte stream. Keepalive, QoS 0/1/2 handshakes, in-flight replay, and resumed
sessions stay inside the client; transport establishment, reconnect timing, and application
dispatch stay outside it.

The main API is [`Session`].

## Scope

Choose Minimq when fixed memory use and a small managed client matter more than protocol
extensibility. It deliberately provides one direct session model rather than a general MQTT packet
toolkit: no allocator, executor, network stack, runtime-sized queues, or application framework.

Downstream users include [`miniconf_mqtt`](https://docs.rs/miniconf_mqtt), which exposes typed
configuration trees over MQTT.

## What You Use

- [`Buffers`]: caller-owned RX/TX memory
- [`ConfigBuilder`]: session configuration
- [`Connection`]: one live MQTT connection
- [`Disconnect`]: graceful disconnect options
- [`Io`]: transport boundary for an established byte stream
- [`Session`]: the client you drive
- [`InboundPublish`]: output of [`Connection::recv()`]

## Example

```no_run
# use std::io;
# struct MyIo;
# use embedded_io_async::{ErrorType, Read, Write};
# impl ErrorType for MyIo {
#     type Error = io::Error;
# }
# impl Read for MyIo {
#     async fn read(&mut self, _buf: &mut [u8]) -> Result<usize, Self::Error> {
#         todo!()
#     }
# }
# impl Write for MyIo {
#     async fn write(&mut self, _buf: &[u8]) -> Result<usize, Self::Error> {
#         todo!()
#     }
#     async fn flush(&mut self) -> Result<(), Self::Error> {
#         todo!()
#     }
# }
# async fn open_io(_addr: SocketAddr) -> Result<MyIo, io::Error> { todo!() }
use core::net::SocketAddr;
use minimq::{Buffers, ConfigBuilder, ConnectEvent, Error, Session, TopicFilter};

async fn run() {
    let rx = &mut [0u8; 256];
    let tx = &mut [0u8; 768];
    let addr: SocketAddr = "127.0.0.1:1883".parse().unwrap();
    let mut session = Session::new(
        ConfigBuilder::new(Buffers::new(rx, tx))
            .client_id("demo")
            .unwrap(),
    );

    loop {
        let io = open_io(addr).await.unwrap();
        // `connect` returns a connection handle that owns the transport and borrows
        // the session. Dropping it at the end of the loop releases both for the next
        // reconnect.
        let mut conn = session.connect(io).await.unwrap();
        match conn.connect_event() {
            ConnectEvent::Connected => {
                conn.subscribe(&[TopicFilter::new("demo/in")], &[])
                    .await
                    .unwrap();
            }
            ConnectEvent::Reconnected => {}
        }

        loop {
            match conn.recv().await {
                Ok(message) => println!("topic={}", message.topic()),
                Err(Error::Disconnected) => break,
                Err(err) => panic!("{err}"),
            }
        }
    }
}

# fn main() {}
```

The attached transport must implement [`embedded_io_async::Read`] and
[`embedded_io_async::Write`].
Ordinary lack of inbound data must keep the read future pending; if the transport returns
`TimedOut` or `Interrupted`, [`Connection::poll()`] treats that as transport failure and
disconnects the connection.

For a TLS MQTT v5 request/reply example that preserves a subscription across reconnects,
see `examples/tls_public_broker.rs`.

## Errors

`ConfigBuilder` reports setup-time validation failures through [`ConfigError`].
Connected session operations report [`Error`]:
- broker rejections and invalid inbound MQTT data surface as [`PeerError`]
- local buffer and capacity limits surface as [`ResourceError`]
- transport failures surface as [`Error::Transport`]

## Session Model

MQTT 5 is the default. To use MQTT 3.1.1, select it on the builder:

```rust
# use minimq::{Buffers, ConfigBuilder, Protocol, Session};
# let mut rx = [0; 256];
# let mut tx = [0; 1024];
let config = ConfigBuilder::new(Buffers::new(&mut rx, &mut tx))
    .client_id("traffic-map")?
    .protocol(Protocol::V3_1_1 { clean_session: true });
let session = Session::new(config);
# Ok::<(), minimq::ConfigError>(())
```

Every connection uses the session's selected protocol. Separate sessions can use different
versions simultaneously.

- MQTT 3.1.1 `clean_session: true` starts fresh on every connection.
- MQTT 3.1.1 `clean_session: false` requests persistent broker state on every connection,
  including the first. It requires a nonempty, stable client ID. Local in-flight messages
  live in the `Session` and are replayed when the broker resumes the session. A fresh
  broker session clears local in-flight state.
- MQTT 5 starts clean on the first connection and requests resumption on reconnect.
  `session_expiry_interval()` controls broker state lifetime after disconnect (default zero).

For MQTT 3.1.1, `connect()` rejects explicitly configured session expiry, automatic QoS downgrade,
or Will properties before writing. Publish/subscribe/unsubscribe properties, MQTT 5 subscription
options, and non-success disconnect reasons are rejected with `Error::InvalidRequest`.
Empty property slices and default subscription options work with either version. Both versions
support username/password authentication.

MQTT 3.1.1 supports QoS 0/1/2 within the configured buffers and local in-flight limits.
Miniconf and MQTT RPC require MQTT 5.

You provide packet buffers plus an already-established transport, and a loop that explicitly
passes that transport into [`Session::connect()`] to establish or resume the broker session.

[`Session::connect()`] takes ownership of the provided transport and performs the unbounded MQTT
`CONNECT` / `CONNACK` handshake. It returns a [`Connection`] that borrows the session. Once
connected:
- [`Connection::recv()`] blocks until the next inbound publish arrives or the connection is lost.
- [`Connection::poll()`] blocks until any session progress happens and returns `Ok(None)` for
  internal-only progress such as ACK handling, replay, or keepalive traffic.

Dropping the connection releases the session for a later reconnect. Call
[`Connection::disconnect()`] first for a graceful MQTT close.

- [`ConnectEvent::Connected`] means the broker created a fresh session. Re-establish subscriptions
  here.
- [`ConnectEvent::Reconnected`] means the broker resumed the existing MQTT session. Existing
  subscriptions and in-flight QoS state were kept.

After a disconnect or transport/protocol error, drop the connection and call
[`Session::connect()`] with a fresh transport when ready to reconnect.

Track each operation handle ([`Op`]) on its originating session until completion or
invalidation, within the packet-identifier reuse window described in its rustdoc.
Handle broker errors reported while driving the connection.

A transport write returning `Ok(0)` for a non-empty buffer produces [`Error::WriteZero`]
and marks the connection disconnected. Drop it to release the transport.

For cooperative driving:
- use [`Connection::drive()`] for immediate local progress without waiting for future inbound reads
  or future session deadlines
- wrap cancel-safe blocking [`Connection::poll()`] or [`Connection::recv()`] in an external timeout
  such as [`embassy_time::with_timeout()`] or [`embassy_time::with_deadline()`]
- if you need real wall-clock limits, enforce them in the transport's `read`, `write`, and
  `flush` futures; using the same budget as minimq's internal MQTT round-trip timeout keeps
  keepalive and transport liveness aligned

## Buffers

You supply two buffers.

- `rx` stores one inbound MQTT packet at a time. Size it for the largest inbound publish,
  including topic, properties, and payload.
- `tx` stores outbound encodes and packed in-flight state. Size it for the retained in-flight bytes
  plus the larger of the `CONNECT` workspace and the largest temporary packet-encoding workspace.
  Minimq keeps enough TX capacity free to reconnect without discarding admitted state.

QoS 2 publications occupy an in-flight slot until `PUBCOMP`; their encoded payload
space is freed on `PUBREC`.

An oversized inbound packet or exhausted inbound QoS 2 storage marks the connection
disconnected. Drop it to release the transport.

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
- [`embassy_time`] for timing
