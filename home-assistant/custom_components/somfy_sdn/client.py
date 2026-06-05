"""WebSocket client for the somfy-sdn Controller API (ws_api.cpp).

Push model: the server sends `state` snapshots on change + a periodic heartbeat. Commands are
JSON {type:"command", command:"...", addr:"AA:BB:CC", id:"..."}; the server replies with
{type:"ack", id:"..."} on success or {type:"error", id:"...", message:"..."} on failure.
Incoming acks/errors are matched to outstanding commands by their generated id.

This is a near-verbatim port of the Actron MITM controller's client — the protocol envelope is
identical; only the command set differs (handled by the caller).
"""

import asyncio
import json
import logging
import uuid
from typing import Any, Awaitable, Callable, Optional

import websockets

from .const import COMMAND_RESPONSE_TIMEOUT_S, CONNECT_TIMEOUT_S

_LOGGER = logging.getLogger(__name__)

StateCallback = Callable[[dict[str, Any]], Awaitable[None]]


class SomfySdnClient:
    """Minimal WS client. No reconnect logic — that's the coordinator's job."""

    def __init__(self, host: str, port: int):
        self.host = host
        self.port = port
        self.uri = f"ws://{host}:{port}"
        self._ws: Optional[websockets.WebSocketClientProtocol] = None
        self._pending: dict[str, asyncio.Future] = {}
        self._on_state: Optional[StateCallback] = None
        self._on_disconnect: Optional[Callable[[], Awaitable[None]]] = None

    async def connect(self):
        # WS-level keepalive is load-bearing: when the ESP32 reboots (e.g. during OTA) it
        # doesn't send a TCP FIN, so without pings the socket would linger. Library defaults:
        # ping every 20 s, fail after 20 s without pong -> ConnectionClosed -> reconnect.
        self._ws = await asyncio.wait_for(
            websockets.connect(self.uri, ping_interval=20, ping_timeout=20),
            timeout=CONNECT_TIMEOUT_S,
        )

    async def close(self):
        if self._ws is not None:
            await self._ws.close()
            self._ws = None
        for fut in self._pending.values():
            if not fut.done():
                fut.cancel()
        self._pending.clear()

    def set_state_callback(self, cb: StateCallback):
        self._on_state = cb

    def set_disconnect_callback(self, cb: Callable[[], Awaitable[None]]):
        self._on_disconnect = cb

    async def run(self):
        """Drain incoming messages until the socket closes."""
        if self._ws is None:
            return
        try:
            async for raw in self._ws:
                try:
                    msg = json.loads(raw)
                except json.JSONDecodeError:
                    _LOGGER.warning("Invalid JSON from server: %r", raw)
                    continue
                await self._handle_message(msg)
        except websockets.ConnectionClosed:
            _LOGGER.info("WebSocket closed by peer")
        finally:
            if self._on_disconnect is not None:
                await self._on_disconnect()

    async def _handle_message(self, msg: dict[str, Any]):
        kind = msg.get("type")
        if kind == "state":
            if self._on_state is not None:
                await self._on_state(msg.get("data") or {})
        elif kind in ("ack", "error"):
            cmd_id = msg.get("id")
            fut = self._pending.pop(cmd_id, None) if cmd_id else None
            if fut is not None and not fut.done():
                fut.set_result(msg)
        else:
            _LOGGER.debug("Ignoring unknown message type: %s", kind)

    async def send_command(self, command: str, **extra) -> dict[str, Any]:
        """Send a command; raise on error / timeout, return the ack payload on success."""
        if self._ws is None:
            raise RuntimeError("Not connected")

        cmd_id = uuid.uuid4().hex
        payload: dict[str, Any] = {"type": "command", "command": command, "id": cmd_id}
        payload.update(extra)

        fut: asyncio.Future = asyncio.get_running_loop().create_future()
        self._pending[cmd_id] = fut

        await self._ws.send(json.dumps(payload))
        try:
            result = await asyncio.wait_for(fut, timeout=COMMAND_RESPONSE_TIMEOUT_S)
        except asyncio.TimeoutError:
            self._pending.pop(cmd_id, None)
            raise RuntimeError(f"Timeout waiting for {command} response")

        if result.get("type") == "error":
            raise RuntimeError(result.get("message", "command rejected"))
        return result
