"""Asynchronous MQTT RPC client."""

from __future__ import annotations

import asyncio
import logging
import math
import uuid
from urllib.parse import urlsplit

from gmqtt import Client as GmqttClient
from gmqtt import Subscription
from gmqtt.mqtt.constants import MQTTv50
from gmqtt.mqtt.handler import MQTTError

LOGGER = logging.getLogger(__name__)
RESPONSE_CODE_PROPERTY = "code"
SUCCESS_CODE = "Ok"


class RemoteError(Exception):
    """Error response returned by the remote service."""

    def __init__(self, code: str, payload: bytes):
        self.code = code
        self.payload = payload
        super().__init__(f"{code}: {payload.decode(errors='replace')}")


class ProtocolError(Exception):
    """Malformed response from the remote service."""


def _first(properties: dict, name: str):
    value = properties.get(name)
    if isinstance(value, list):
        return value[0] if value else None
    return value


def _broker_address(broker: str) -> tuple[str, int]:
    if broker.count(":") > 1 and not broker.startswith("["):
        return broker, 1883
    parsed = urlsplit(f"//{broker}")
    if parsed.hostname is None:
        raise ValueError("broker must be a hostname or IP address")
    return parsed.hostname, 1883 if parsed.port is None else parsed.port


class Client:
    """One connected MQTT RPC requester with an exclusive MQTT session."""

    def __init__(
        self,
        broker: str,
        prefix: str,
        *,
        client_id: str | None = None,
        keepalive: int = 60,
    ):
        self.broker = broker
        self.prefix = prefix
        self._topic_prefix = f"{prefix}/"
        self.response_topic = f"{self._topic_prefix}response/{uuid.uuid4().hex}"
        self.keepalive = keepalive
        self._mqtt = GmqttClient(client_id or f"mqtt-rpc-py-{uuid.uuid4().hex}")
        self._mqtt.on_message = self._on_message
        self._mqtt.on_subscribe = self._on_subscribe
        self._suback: asyncio.Future[tuple[int, ...]] | None = None
        self._inflight: dict[bytes, asyncio.Future[bytes]] = {}
        self._connected = False

    async def __aenter__(self) -> Client:
        host, port = _broker_address(self.broker)
        await self._mqtt.connect(host, port, keepalive=self.keepalive, version=MQTTv50)
        self._connected = True
        try:
            await self._subscribe()
        except BaseException:
            await self.close()
            raise
        return self

    async def __aexit__(self, *_exc_info) -> None:
        await self.close()

    async def close(self) -> None:
        """Cancel outstanding requests and disconnect."""

        if not self._connected:
            return
        self._connected = False
        for future in self._inflight.values():
            future.cancel()
        await self._mqtt.disconnect()

    async def request(
        self,
        method: str,
        payload: str | bytes = b"",
        *,
        timeout: float = 3.0,
    ) -> bytes:
        """Call one method and return its successful response payload."""

        if not self._connected:
            raise MQTTError("MQTT RPC client is not connected")
        if not method:
            raise ValueError("method must not be empty")
        if timeout <= 0 or not math.isfinite(timeout):
            raise ValueError("timeout must be positive and finite")

        correlation = uuid.uuid4().bytes
        future = asyncio.get_running_loop().create_future()
        self._inflight[correlation] = future
        properties = {
            "response_topic": self.response_topic,
            "correlation_data": correlation,
            "message_expiry_interval": max(1, math.ceil(timeout)),
        }
        topic = f"{self._topic_prefix}rpc/{method}"
        LOGGER.debug("Publishing request to %s", topic)
        self._mqtt.publish(topic, payload, qos=1, retain=False, **properties)
        try:
            return await asyncio.wait_for(future, timeout)
        finally:
            self._inflight.pop(correlation, None)

    async def _subscribe(self) -> None:
        subscription = Subscription(
            self.response_topic,
            qos=1,
            no_local=True,
            retain_as_published=False,
            retain_handling_options=2,
        )
        future = asyncio.get_running_loop().create_future()
        self._suback = future
        try:
            self._mqtt.subscribe(subscription)
            reasons = await asyncio.wait_for(future, 3.0)
        finally:
            self._suback = None
        if not reasons or reasons[0] >= 128:
            raise MQTTError(f"SUBACK failed for {self.response_topic}: {reasons}")

    def _on_subscribe(
        self,
        _client: GmqttClient,
        _mid: int,
        reasons: tuple[int, ...],
        _properties: dict,
    ) -> None:
        if (future := self._suback) is not None and not future.done():
            future.set_result(reasons)

    def _on_message(
        self,
        _client: GmqttClient,
        topic: str,
        payload: bytes,
        _qos: int,
        properties: dict,
    ) -> None:
        if topic != self.response_topic:
            return
        correlation = _first(properties, "correlation_data")
        future = self._inflight.get(correlation)
        if future is None or future.done():
            LOGGER.debug("Discarding response with unknown correlation data")
            return
        try:
            codes = [
                value
                for name, value in properties.get("user_property", ())
                if name == RESPONSE_CODE_PROPERTY
            ]
        except (TypeError, ValueError):
            codes = []
        if len(codes) != 1:
            future.set_exception(ProtocolError("response must carry exactly one code"))
        elif codes[0] == SUCCESS_CODE:
            future.set_result(payload)
        else:
            future.set_exception(RemoteError(codes[0], payload))
