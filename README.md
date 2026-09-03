# Minimq workspace

Small, allocation-free MQTT 5 building blocks for embedded systems:

- [`minimq`](minimq/) is the async MQTT 5 client.
- [`mqtt-rpc`](mqtt-rpc/) is a minimal MQTT 5 request/reply protocol with a
  matching Python client.
- [`mqtt-staging`](mqtt-staging/) stages bounded objects into
  application-owned storage and includes a matching Python sender.

The packages have independent versions and release boundaries. The protocol
crates use Minimq's public API and do not own application dispatch, storage,
activation, or reboot policy.
