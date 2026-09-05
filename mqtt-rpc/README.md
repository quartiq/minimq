# mqtt-rpc

Minimal `no_std` MQTT 5 request/response transport for embedded services.

`mqtt-rpc` works with a caller-owned Minimq 0.13 session. It subscribes
`<device-prefix>/rpc/#`, routes borrowed inbound requests, retains MQTT response-topic and
correlation data for deferred work, and publishes transient QoS-1 responses.

It deliberately does not own the MQTT transport, application method registry, payload schema,
executor, persistent storage, or retained device status.

The [`py`](py/) directory contains the corresponding Python requester and command-line client.

## Wire contract

- Requests are published below `<device-prefix>/rpc/` and must carry an MQTT 5 Response Topic.
- Correlation Data is optional and copied unchanged to the response.
- Retained requests are rejected and never dispatched.
- Responses are QoS 1, non-retained, and carry one `code` User Property. `Ok` means success;
  applications define other codes.
- Request and response payloads are opaque to MQTT RPC.
- QoS 1 provides at-least-once delivery. Application methods with side effects must be idempotent;
  Correlation Data routes replies but is not a durable deduplication record.

The requester chooses the MQTT response topic. Before responding, the application
can inspect `ResponseTarget::topic()` and must rely on an appropriate broker ACL
or reject targets outside its allowed response-topic tree.

## Device use

After every successful `Session::connect`, tell the service whether the broker resumed the MQTT
session and drive its subscription to completion. Then pass each inbound publication to `handle`:

```rust,no_run
use minimq::{Connection, Io};
use mqtt_rpc::{Handle, Service, SUCCESS_CODE, respond};

async fn run<IO: Io>(connection: &mut Connection<'_, '_, IO>) {
    let mut rpc = Service::new("dt/device").unwrap();
    rpc.begin_connection(connection.connect_event());
    while !rpc.step(connection).await.unwrap() {
        let _ = connection.poll().await.unwrap();
    }

    loop {
        let inbound = connection.recv().await.unwrap();
        if let Handle::Request(request) = rpc.handle(&inbound) {
            assert_eq!(request.method(), "ping");
            let target = request.into_response_target();
            respond(connection, &target, SUCCESS_CODE, b"pong".as_slice())
                .await
                .unwrap();
        }
    }
}
```
