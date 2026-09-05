from __future__ import annotations

import asyncio
import unittest

from mqtt_staging import (
    Status,
    Transfer,
    aligned_chunk_size,
    broker_endpoint,
    chunk_properties,
    json_manifest,
    wait_status,
)

PREFIX = "devices/example/staging"
TRANSFER_ID = "85944171f73967e8"


class Message:
    def __init__(
        self,
        payload: bytes,
        properties,
        topic: str = f"{PREFIX}/status",
    ) -> None:
        self.topic = topic
        self.payload = payload
        self.properties = properties


def status_properties():
    return {
        "response_topic": f"{PREFIX}/chunk",
        "correlation_data": TRANSFER_ID.encode(),
        "user_property": [("offset", "1024")],
    }


class HostToolTests(unittest.TestCase):
    def test_aligned_chunk_size_fits_mtu_and_write_size(self) -> None:
        self.assertEqual(aligned_chunk_size(1500, 1024, 32), 1024)
        self.assertEqual(aligned_chunk_size(1000, 1024, 32), 992)
        self.assertEqual(aligned_chunk_size(64, 16, 32), 0)

    def test_broker_endpoint(self) -> None:
        self.assertEqual(broker_endpoint("mqtt"), ("mqtt", 1883))
        self.assertEqual(broker_endpoint("mqtt.example:1884"), ("mqtt.example", 1884))
        self.assertEqual(broker_endpoint("[::1]:1884"), ("::1", 1884))
        self.assertEqual(
            broker_endpoint("mqtt://mqtt.example:1884"),
            ("mqtt.example", 1884),
        )

    def test_status_parses_receipt_payload(self) -> None:
        status = Status.from_message(
            Message(
                b'{"state":"ready","code":"accepted","id":"85944171f73967e8",'
                b'"next_offset":1024,"size":2048,"mtu":1024,"write_size":32}',
                status_properties(),
            )
        )

        self.assertEqual(status.state, "ready")
        self.assertEqual(status.code, "accepted")
        self.assertEqual(status.id, TRANSFER_ID)
        self.assertEqual(status.response_topic, f"{PREFIX}/chunk")
        self.assertEqual(status.correlation_data, TRANSFER_ID.encode())
        self.assertEqual(status.offset, 1024)
        self.assertEqual(status.next_offset, 1024)
        self.assertEqual(status.size, 2048)
        self.assertEqual(status.mtu, 1024)
        self.assertEqual(status.write_size, 32)

    def test_status_raises_on_rejection(self) -> None:
        status = Status.from_message(
            Message(
                b'{"state":"error","code":"offset","id":"85944171f73967e8",'
                b'"next_offset":0,"size":2048,"mtu":1024,"write_size":32}',
                status_properties(),
            )
        )

        with self.assertRaisesRegex(RuntimeError, "rejected"):
            status.raise_for_error()

    def test_status_raises_on_mtu_rejection(self) -> None:
        status = Status.from_message(
            Message(
                b'{"state":"idle","code":"mtu","id":"",'
                b'"next_offset":0,"size":0,"mtu":1024,"write_size":32}',
                status_properties(),
            )
        )

        with self.assertRaisesRegex(RuntimeError, "rejected"):
            status.raise_for_error()

    def test_chunk_properties_echo_request_correlation_and_offset(self) -> None:
        properties = chunk_properties(b"opaque", 37)

        self.assertEqual(properties["payload_format_id"], 0)
        self.assertEqual(properties["correlation_data"], b"opaque")
        self.assertEqual(properties["user_property"], [("offset", "37")])

    def test_json_manifest_is_compact_and_deterministic(self) -> None:
        manifest = json_manifest(b"foobar")

        self.assertEqual(
            manifest,
            b'{"id":"85944171f73967e8","size":6,"fnv1a64":9625390261332436968}',
        )


class HostToolAsyncTests(unittest.IsolatedAsyncioTestCase):
    async def test_wait_status_ignores_another_transfer(self) -> None:
        messages = asyncio.Queue()
        properties = status_properties()
        await messages.put(
            Message(
                b'{"state":"ready","code":"accepted","id":"other",'
                b'"next_offset":0,"size":2048,"mtu":1024,"write_size":32}',
                properties,
            )
        )
        await messages.put(
            Message(
                b'{"state":"ready","code":"accepted","id":"85944171f73967e8",'
                b'"next_offset":1024,"size":2048,"mtu":1024,"write_size":32}',
                properties,
            )
        )

        status = await wait_status(
            messages,
            f"{PREFIX}/status",
            Transfer(TRANSFER_ID, 2048),
            timeout=0.1,
        )

        self.assertEqual(status.id, TRANSFER_ID)
        self.assertEqual(status.next_offset, 1024)


if __name__ == "__main__":
    unittest.main()
