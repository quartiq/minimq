# `mqtt-rpc` Python client

Python 3.11+ client for MQTT 5 request/response services implemented with the MQTT RPC Rust crate.

```sh
python -m pip install mqtt-rpc
mqtt-rpc --broker mqtt dt/device settings/store
```

The async API carries opaque `str` or `bytes` request payloads and returns response bytes:

```python
from mqtt_rpc.client import Client

async with Client("mqtt", "dt/device") as rpc:
    response = await rpc.request("settings/store")
```

Each client uses a dedicated MQTT connection and response topic. Requests are QoS 1, non-retained,
and carry MQTT Response Topic, Correlation Data, and Message Expiry properties. Responses must copy
the Correlation Data and carry exactly one `code` User Property. `Ok` returns the payload; any other
code raises `RemoteError`.

QoS 1 provides at-least-once delivery. Methods with side effects must be idempotent; Correlation
Data routes replies but is not a durable deduplication record.

MQTT RPC does not provide device discovery, schemas, serialization, persistence policy, or retained
status. Applications compose those separately.
