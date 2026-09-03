# Minimq workspace

Opinionated, allocation-free MQTT 5 components for embedded systems:

- [`minimq`](minimq/) is the async MQTT 5 client.
- [`mqtt-rpc`](mqtt-rpc/) is a minimal MQTT 5 request/reply protocol with a
  matching Python client.
- [`mqtt-staging`](mqtt-staging/) stages bounded objects into
  application-owned storage and includes a matching Python sender.

Minimq owns MQTT session mechanics, not the network stack or application. The
protocol crates likewise leave dispatch, storage, activation, and reboot policy
to their applications. Each package has an independent version and release
boundary.
