"""Host-side sender for the mqtt-staging protocol."""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import os
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from gmqtt import Client
from gmqtt.mqtt.constants import MQTTv50
from gmqtt.mqtt.handler import MQTTError

LOGGER = logging.getLogger(__name__)
FNV1A64_OFFSET = 0xCBF29CE484222325
FNV1A64_PRIME = 0x100000001B3
INFO_CHUNK_STRIDE = 128
QOS_AT_LEAST_ONCE = 1
MqttProperties = dict[str, Any]


@dataclass(frozen=True, slots=True)
class Message:
    """One MQTT PUBLISH received by the staging command."""

    topic: str
    payload: bytes
    properties: MqttProperties


@dataclass(frozen=True, slots=True)
class Transfer:
    """Identity of the manifest being served."""

    id: str
    size: int

    @classmethod
    def from_manifest(cls, manifest: bytes) -> Transfer:
        data = json.loads(manifest)
        return cls(data["id"], data["size"])

    def matches(self, status: Status) -> bool:
        return (
            status.id == self.id
            and status.correlation_data == self.id.encode()
            and status.size == self.size
        )


class MqttClient:
    """Small gmqtt adapter used by the staging command."""

    def __init__(self, broker: str):
        host, port = broker_endpoint(broker)
        self.host = host
        self.port = port
        self.messages: asyncio.Queue[Message] = asyncio.Queue()
        self._client = Client(f"mqtt-staging-{uuid.uuid4().hex}", clean_session=True)
        self._client.on_message = self._on_message
        self._client.on_subscribe = self._on_subscribe
        self._subacks: dict[int, asyncio.Future[tuple[int, ...]]] = {}

    async def __aenter__(self) -> MqttClient:
        await self._client.connect(self.host, self.port, version=MQTTv50)
        return self

    async def __aexit__(self, *_exc_info) -> None:
        await self._client.disconnect()

    async def subscribe(self, topic: str, *, qos: int) -> None:
        mid = self._client.subscribe(topic, qos=qos)
        if mid is None:
            return
        future = asyncio.get_running_loop().create_future()
        self._subacks[mid] = future
        reasons = await asyncio.wait_for(future, 3.0)
        if not reasons or reasons[0] >= 128:
            raise MQTTError(f"SUBACK failed for {topic}: {reasons}")

    async def publish(
        self,
        topic: str,
        payload: bytes,
        *,
        qos: int,
        properties: MqttProperties,
    ) -> None:
        self._client.publish(topic, payload, qos=qos, **properties)
        await asyncio.sleep(0)

    def _on_message(
        self,
        _client: Client,
        topic: str,
        payload: bytes,
        _qos: int,
        properties: MqttProperties,
    ) -> None:
        self.messages.put_nowait(Message(topic, payload, properties))

    def _on_subscribe(
        self,
        _client: Client,
        mid: int,
        reasons: tuple[int, ...],
        _properties: MqttProperties,
    ) -> None:
        if future := self._subacks.pop(mid, None):
            future.set_result(reasons)


@dataclass(frozen=True, slots=True)
class Status:
    """One device staging status receipt."""

    state: str
    code: str
    id: str
    response_topic: str
    correlation_data: bytes
    offset: int
    next_offset: int
    size: int
    mtu: int
    write_size: int

    @classmethod
    def from_message(cls, message) -> Status:
        data = json.loads(message.payload)
        properties = message.properties
        user_properties = dict(properties.get("user_property", ()))
        return cls(
            state=data["state"],
            code=data["code"],
            id=data["id"],
            response_topic=_first(properties, "response_topic"),
            correlation_data=_first(properties, "correlation_data"),
            offset=int(user_properties["offset"]),
            next_offset=data["next_offset"],
            size=data["size"],
            mtu=data["mtu"],
            write_size=data["write_size"],
        )

    def raise_for_error(self) -> None:
        if self.state == "error" or self.code in {
            "error",
            "mtu",
            "offset",
            "oversize",
            "storage",
            "unaligned",
        }:
            raise RuntimeError(f"device rejected staging: {self}")


def _first(properties: MqttProperties, name: str, default=None):
    value = properties.get(name, default)
    if isinstance(value, list):
        return value[0] if value else default
    return value


def aligned_chunk_size(requested: int, mtu: int, write_size: int) -> int:
    """Choose a chunk size that meets the device write alignment."""

    size = min(requested, mtu)
    return size - size % write_size


def chunk_properties(correlation_data: bytes, offset: int) -> MqttProperties:
    return {
        "payload_format_id": 0,
        "correlation_data": correlation_data,
        "user_property": [("offset", str(offset))],
    }


def should_log_chunk_progress(
    *, offset: int, payload_len: int, size: int, chunk_size: int
) -> bool:
    if offset == 0:
        return True
    if offset + payload_len >= size:
        return True
    if chunk_size <= 0:
        return False
    return (offset // chunk_size) % INFO_CHUNK_STRIDE == 0


def fnv1a64(data: bytes) -> int:
    digest = FNV1A64_OFFSET
    for byte in data:
        digest ^= byte
        digest = digest * FNV1A64_PRIME & 0xFFFFFFFFFFFFFFFF
    return digest


def json_manifest(data: bytes) -> bytes:
    checksum = fnv1a64(data)
    manifest = {
        "id": f"{checksum:016x}",
        "size": len(data),
        "fnv1a64": checksum,
    }
    return json.dumps(manifest, separators=(",", ":")).encode()


def broker_endpoint(broker: str) -> tuple[str, int]:
    """Normalize one MQTT broker address to host and port."""

    endpoint = urlsplit(broker if "://" in broker else f"mqtt://{broker}")
    return endpoint.hostname or broker, endpoint.port or 1883


async def wait_status(
    messages: asyncio.Queue[Message],
    topic: str,
    transfer: Transfer,
    *,
    timeout: float,
) -> Status:
    """Wait for the next status/request receipt."""

    async with asyncio.timeout(timeout):
        while True:
            message = await messages.get()
            if message.topic != topic:
                continue
            status = Status.from_message(message)
            if not status.id and not status.correlation_data:
                status.raise_for_error()
                continue
            if not transfer.matches(status):
                LOGGER.debug("ignoring status for another staging transfer")
                continue
            status.raise_for_error()
            return status
    raise TimeoutError(f"timed out waiting for {topic}")


async def stage(
    *,
    broker: str,
    prefix: str,
    manifest: bytes,
    data: bytes,
    chunk_size: int,
    prepare_timeout: float,
    timeout: float,
) -> None:
    """Stage one object through a device's MQTT service."""

    host, port = broker_endpoint(broker)
    LOGGER.info(
        "staging broker=%s:%s prefix=%s size=%dB chunk=%dB",
        host,
        port,
        prefix,
        len(data),
        chunk_size,
    )
    transfer = Transfer.from_manifest(manifest)
    async with MqttClient(broker) as client:
        status_topic = f"{prefix}/status"
        manifest_topic = f"{prefix}/manifest"
        messages = client.messages
        LOGGER.info("subscribing status topic %s", status_topic)
        await client.subscribe(status_topic, qos=QOS_AT_LEAST_ONCE)
        LOGGER.info("publishing manifest topic %s", manifest_topic)
        await client.publish(
            manifest_topic,
            manifest,
            qos=QOS_AT_LEAST_ONCE,
            properties={"payload_format_id": 1},
        )
        preparing = True
        while True:
            status = await wait_status(
                messages,
                status_topic,
                transfer,
                timeout=prepare_timeout if preparing else timeout,
            )
            LOGGER.debug(
                (
                    "status state=%s code=%s offset=%s next=%s/%s "
                    "mtu=%s write=%s response=%s correlation=%dB"
                ),
                status.state,
                status.code,
                status.offset,
                status.next_offset,
                status.size,
                status.mtu,
                status.write_size,
                status.response_topic,
                len(status.correlation_data or b""),
            )
            if status.code == "complete":
                LOGGER.info("staging complete size=%s", status.size)
                return
            if (
                status.response_topic != f"{prefix}/chunk"
                or status.offset != status.next_offset
            ):
                raise RuntimeError(f"invalid staging chunk request: {status}")
            if status.state == "preparing":
                LOGGER.info(
                    "status state=%s code=%s next=%s/%s",
                    status.state,
                    status.code,
                    status.next_offset,
                    status.size,
                )
                continue
            preparing = False
            if should_log_chunk_progress(
                offset=status.offset,
                payload_len=max(status.next_offset - status.offset, 0),
                size=status.size,
                chunk_size=max(status.mtu, 1),
            ):
                LOGGER.info(
                    "status code=%s offset=%s next=%s/%s mtu=%s write=%s",
                    status.code,
                    status.offset,
                    status.next_offset,
                    status.size,
                    status.mtu,
                    status.write_size,
                )
            size = aligned_chunk_size(chunk_size, status.mtu, status.write_size)
            if size == 0:
                raise RuntimeError(
                    "device MTU cannot fit one aligned write: "
                    f"mtu={status.mtu} write_size={status.write_size}"
                )
            offset = status.offset
            chunk = data[offset : offset + size]
            if not chunk:
                raise RuntimeError(f"device requested empty chunk at {offset}")
            raw_len = len(chunk)
            if offset + len(chunk) >= len(data):
                chunk += b"\xff" * ((-len(chunk)) % status.write_size)
            if should_log_chunk_progress(
                offset=offset,
                payload_len=raw_len,
                size=len(data),
                chunk_size=max(size, 1),
            ):
                LOGGER.info(
                    "publishing chunk offset=%s raw=%sB payload=%sB",
                    offset,
                    raw_len,
                    len(chunk),
                )
            else:
                LOGGER.debug(
                    "publishing chunk offset=%s raw=%sB payload=%sB",
                    offset,
                    raw_len,
                    len(chunk),
                )
            await client.publish(
                status.response_topic,
                chunk,
                qos=QOS_AT_LEAST_ONCE,
                properties=chunk_properties(status.correlation_data, offset),
            )


async def async_main() -> None:
    parser = argparse.ArgumentParser(
        description="Stage one object over MQTT.",
    )
    parser.add_argument("-v", "--verbose", action="count", default=0)
    parser.add_argument(
        "-b",
        "--broker",
        default=os.environ.get("BROKER", "localhost:1883"),
    )
    parser.add_argument(
        "-p",
        "--prefix",
        required=True,
        help="complete protocol topic root, for example devices/example/staging",
    )
    parser.add_argument("--file", type=Path, required=True)
    parser.add_argument("--chunk-size", type=int, default=1024)
    parser.add_argument("--prepare-timeout", type=float, default=30.0)
    parser.add_argument("--timeout", type=float, default=10.0)
    args = parser.parse_args()

    logging.basicConfig(
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
        level=logging.INFO if args.verbose == 0 else logging.DEBUG,
    )

    data = args.file.read_bytes()
    manifest = json_manifest(data)

    await stage(
        broker=args.broker,
        prefix=args.prefix,
        manifest=manifest,
        data=data,
        chunk_size=args.chunk_size,
        prepare_timeout=args.prepare_timeout,
        timeout=args.timeout,
    )


def main() -> None:
    asyncio.run(async_main())


if __name__ == "__main__":
    main()
