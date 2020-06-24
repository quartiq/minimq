# minimq

Minimal no_std MQTT v5.0 client implementation.

For now, see `src/bin/integration_test.rs` for usage.

## Setup

```
$ nix-shell       # get a rust environment with ejabberd for testing
$ make ejabberd & # run MQTT broker in background
$ cargo test      # run all the tests
```

## Todo/NYI

- Should communicate `PACKET_MAX` to broker in *CONNECT* properties
- SUBACK/QoS=1 for inbound messages
- Batch SUBSCRIBE
- Keepalive/timeouts
- Reconsider *PubInfo* and/or API in general
- Docs
