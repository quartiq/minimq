"""MQTT RPC command-line client."""

import argparse
import asyncio

from gmqtt.mqtt.handler import MQTTError

from .client import Client, ProtocolError, RemoteError


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Call an MQTT RPC method")
    parser.add_argument("--broker", default="localhost")
    parser.add_argument("--timeout", type=float, default=3.0)
    parser.add_argument("prefix")
    parser.add_argument("method")
    parser.add_argument("payload", nargs="?", default="")
    return parser


async def _run(args: argparse.Namespace) -> bytes:
    async with Client(args.broker, args.prefix) as rpc:
        return await rpc.request(args.method, args.payload, timeout=args.timeout)


def main() -> None:
    parser = _parser()
    args = parser.parse_args()
    try:
        response = asyncio.run(_run(args))
    except (MQTTError, ProtocolError, RemoteError, TimeoutError, ValueError) as error:
        parser.exit(1, f"mqtt-rpc: {error}\n")
    if response:
        print(response.decode(errors="replace"))


if __name__ == "__main__":
    main()
