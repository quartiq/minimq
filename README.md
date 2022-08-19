[![QUARTIQ Matrix Chat](https://img.shields.io/matrix/quartiq:matrix.org)](https://matrix.to/#/#quartiq:matrix.org)
[![Continuous Integration](https://github.com/quartiq/minimq/actions/workflows/ci.yml/badge.svg)](https://github.com/quartiq/minimq/actions/workflows/ci.yml)

# Minimq

Minimq provides a minimal MQTTv5 client and message parsing for the MQTT version 5 protocol. It
leverages the [`embedded-nal`](https://github.com/rust-embedded-community/embedded-nal) to operate
on top of any TCP stack implementation and is actively used with both
[`smoltcp`](https://github.com/smoltcp-rs/smoltcp) and and the W5500 hardware network stack.

Minimq provides a simple, `no_std` interface to connect to an MQTT broker to publish messages and
subscribe to topics.

## Features

Minimq supports all of the fundamental operations of MQTT, such as message subscription and
publication. Below is a detailed list of features, indicating what aspects are supported and what
aren't:
* Will messages are fully supported.
* Message subscription is fully supported.
* Message publication is supported for all quality-of-service (QoS) levels
* Message reception is only supported for the following QoS levels:
    1. At Most once (Level 0)

If there are features that you would like to have that are not yet supported, we are always
accepting pull requests to extend Minimq's capabilities.

### Smoltcp Support
If using `smoltcp`, check out the [`smoltcp-nal`](https://github.com/quartiq/smoltcp-nal) to quickly
create an interface that can be used by Minimq.

## Examples

An example usage of Minimq that can be run on a desktop PC can be found in
[`tests/integration_test.rs`](https://github.com/quartiq/minimq/blob/master/tests/integration_test.rs)
