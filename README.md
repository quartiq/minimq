# minimq

Minimal `no_std` MQTT v5.0 client implementation.

## Usage

There is an example targeting the Nucleo-H743zi2 board that can be used as a reference design.

There is example usage on a standard computer in `tests/integration_test.rs`

## Todo/NYI

- Support all QoS levels
- Properly manage session state
- Implement keepalive timeouts
- Allow batch subscriptions to multiple topics
- Docs
