import asyncio
import os
import uuid

import pytest
from gmqtt import Client as GmqttClient
from gmqtt import Subscription
from gmqtt.mqtt.constants import MQTTv50
from mqtt_rpc.client import Client, RemoteError


def _first(properties, name):
    value = properties.get(name)
    return value[0] if isinstance(value, list) else value


@pytest.mark.parametrize("trailing_slash", [False, True])
def test_request_response(trailing_slash):
    broker = os.getenv("BROKER")
    if broker is None:
        pytest.skip("set BROKER=host:port to run the broker test")
    asyncio.run(_request_response(broker, trailing_slash))


async def _request_response(broker, trailing_slash):
    host, port = broker.rsplit(":", 1)
    prefix = f"mqtt-rpc-python-test-{uuid.uuid4().hex}"
    service_prefix = f"{prefix}/" if trailing_slash else prefix
    rpc_topic = f"{service_prefix}/rpc"
    subscribed = asyncio.Event()
    device = GmqttClient(f"{prefix}-device")

    def on_subscribe(_client, _mid, _reasons, _properties):
        subscribed.set()

    def on_message(client, topic, payload, _qos, properties):
        assert topic in {f"{rpc_topic}/ping", f"{rpc_topic}//ping"}
        code = "Failed" if payload == b"fail" else "Ok"
        client.publish(
            _first(properties, "response_topic"),
            b"failed" if payload == b"fail" else b"pong",
            qos=1,
            correlation_data=_first(properties, "correlation_data"),
            user_property=[("code", code)],
        )

    device.on_subscribe = on_subscribe
    device.on_message = on_message
    await device.connect(host, int(port), version=MQTTv50)
    device.subscribe(Subscription(f"{rpc_topic}/#", qos=1))
    await asyncio.wait_for(subscribed.wait(), 3.0)
    try:
        async with Client(broker, service_prefix) as rpc:
            assert await rpc.request("ping", b"ping") == b"pong"
            assert await rpc.request("/ping", b"ping") == b"pong"
            with pytest.raises(RemoteError, match="Failed: failed"):
                await rpc.request("ping", b"fail")
    finally:
        await device.disconnect()
