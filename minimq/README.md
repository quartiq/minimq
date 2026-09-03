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

For a TLS MQTT v5 request/reply example that preserves a subscription across reconnects and reuses
the TLS record buffers, see `examples/tls_public_broker.rs`.

## Errors

`ConfigBuilder` reports setup-time validation failures through [`ConfigError`].
Connected session operations report [`Error`]:
- broker rejections and invalid inbound MQTT data surface as [`PeerError`]
- local buffer and capacity limits surface as [`ResourceError`]
- transport failures surface as [`Error::Transport`]

## Session Model

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
- [`Connection::recv()`] yields one inbound publish.

If [`Connection::recv()`] or [`Connection::poll()`] returns [`Error::Disconnected`], the caller
discards the handle and decides
when to call
[`Session::connect()`] with a fresh transport again.
Other transport/protocol errors mark the handle dead; callers should handle the error and reconnect
rather than retrying network operations on that handle.

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
