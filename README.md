[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

Minimq provides a minimal MQTTv5 client and message parsing for the MQTT version 5 protocol. It
now separates MQTT session logic from transport setup. The core client uses explicit caller-owned
buffers and async byte streams via
[`embedded-io-async`](https://docs.rs/embedded-io-async/latest/embedded_io_async/), while the
provided transport adapters target
[`embedded-nal-async`](https://docs.rs/embedded-nal-async/latest/embedded_nal_async/).

Minimq provides a simple, `no_std` interface to connect to an MQTT broker to publish messages and
subscribe to topics.

## Features

Minimq supports all of the fundamental operations of MQTT, such as message subscription and
publication. Below is a detailed list of features, indicating what aspects are supported:

* Publication at all quality-of-service levels (at-most-once, at-least-once, and exactly-once)
* Retained messages
* Connection will messages
* Session state reconnection and republication
* Topic subscriptions at all quality-of-service levels
* Subscription option flags
* Zero-copy message deserialization
* Serde-compatible MQTT message serialization and deserialization

If there are features that you would like to have that are not yet supported, we are always
accepting pull requests to extend Minimq's capabilities.

Minimq also provides convenient APIs to implement request-response interfaces over MQTT leveraging
the `ResponseTopic` and `CorrelationData` properties for in-bound and out-bound messages.

## Transport Model

The crate is split into:

* MQTT client/session logic
* Broker endpoint configuration
* Async transport adapters
* A stateful async runner that owns reconnect and socket lifecycle

The main configuration surface takes explicit buffers:

* `Buffers::rx` for inbound packet data
* `Buffers::tx` for outbound packet encoding
* `Buffers::inflight` for retransmission storage

If a single shared byte slab is still convenient for a target, use `BufferLayout::split()` as a
fallible helper instead of building layout assumptions into the client API.

`Runner` owns the live transport connection. Call `poll()` to drive reconnect, keepalive, and
inbound message delivery, and call `publish()` / `subscribe()` on the runner after the borrowed
inbound message has been dropped.

## Examples

The deterministic protocol regression tests live in
[`tests/async_client.rs`](https://github.com/quartiq/minimq/blob/master/tests/async_client.rs).

Real broker smoke tests live in
[`tests/real_broker.rs`](https://github.com/quartiq/minimq/blob/master/tests/real_broker.rs).
They are enabled by providing broker coordinates through environment variables:

* `MINIMQ_REAL_BROKER_ADDR=127.0.0.1:1883` for socket-address coverage
* `MINIMQ_REAL_BROKER_HOST=localhost` to additionally exercise hostname-plus-DNS transport
  resolution
