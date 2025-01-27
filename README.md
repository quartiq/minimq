[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

Minimq provides a minimal MQTTv5 client and message parsing for the MQTT version 5 protocol. It
leverages the [`embedded-nal`](https://github.com/rust-embedded-community/embedded-nal) to operate
on top of any TCP stack implementation and is actively used with `std-embedded-nal`,
[`smoltcp`](https://github.com/smoltcp-rs/smoltcp), and the W5500 hardware network stack.

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

### Smoltcp Support

If using `smoltcp`, check out the [`smoltcp-nal`](https://github.com/quartiq/smoltcp-nal) to quickly
create an interface that can be used by Minimq.

## Examples

An example usage of Minimq that can be run on a desktop PC can be found in
[`tests/integration_test.rs`](https://github.com/quartiq/minimq/blob/master/tests/integration_test.rs)
