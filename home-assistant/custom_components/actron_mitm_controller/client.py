"""WebSocket client for the actron-sniffer Controller API (ws_api.cpp).

Push model: server sends `state` messages on change + periodic heartbeats. Commands are
JSON {type:"command", command:"...", value:..., id:"..."}; the server replies with
{type:"ack", id:"..."} on success or {type:"error", id:"...", message:"..."} on failure.
Match incoming acks/errors to outstanding commands by their generated id.
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
DiagCallback = Callable[[dict[str, Any]], Awaitable[None]]
HealthCallback = Callable[[dict[str, Any]], Awaitable[None]]


class ActronMitmClient:
    """Minimal WS client. No reconnect logic — that's the coordinator's job."""

    def __init__(self, host: str, port: int):
        self.host = host
        self.port = port
        self.uri = f"ws://{host}:{port}"
        self._ws: Optional[websockets.WebSocketClientProtocol] = None
        self._pending: dict[str, asyncio.Future] = {}
        self._on_state: Optional[StateCallback] = None
        self._on_diag: Optional[DiagCallback] = None
        self._on_health: Optional[HealthCallback] = None
        self._on_message: Optional[Callable[[], None]] = None
        self._on_disconnect: Optional[Callable[[str, Any, float], Awaitable[None]]] = None
        self._opened_at: float = 0.0

    async def connect(self):
        # Keepalive is load-bearing: when the ESP32 reboots (e.g. during OTA) it doesn't
        # send a TCP FIN, so the client-side socket would linger open indefinitely without
        # WS-level pings. The server's 10s state-push heartbeat is server→client only and
        # doesn't help us detect a dead server. Library defaults: ping every 20s, fail
        # after 20s without pong → ConnectionClosed → finally → _on_disconnect → reconnect.
        # Wrap with a connect timeout so we don't hang trying to reach a dead host.
        self._ws = await asyncio.wait_for(
            websockets.connect(self.uri, ping_interval=20, ping_timeout=20),
            timeout=CONNECT_TIMEOUT_S,
        )

    async def close(self):
        if self._ws is not None:
            await self._ws.close()
            self._ws = None
        # Cancel any in-flight commands so callers don't hang forever
        for fut in self._pending.values():
            if not fut.done():
                fut.cancel()
        self._pending.clear()

    def set_state_callback(self, cb: StateCallback):
        self._on_state = cb

    def set_diag_callback(self, cb: DiagCallback):
        self._on_diag = cb

    def set_health_callback(self, cb: HealthCallback):
        self._on_health = cb

    def set_message_callback(self, cb: Callable[[], None]):
        """Called for EVERY inbound frame, whatever its type.

        Availability keys off "have we heard anything at all", not "have we had state". A `health`
        or `diag` frame proves the socket is alive just as well as a state push does, and treating
        it as such stops a device that is talking to us being marked unavailable (ledger
        shq-suite-0038).
        """
        self._on_message = cb

    def set_disconnect_callback(self, cb: Callable[[str, Any, float], Awaitable[None]]):
        """cb(kind, close_code, lifetime_s) — `kind` is one of clean/closed/error."""
        self._on_disconnect = cb

    async def run(self):
        """Drain incoming messages until the socket closes."""
        if self._ws is None:
            return
        started = asyncio.get_running_loop().time()
        self._opened_at = started
        kind = "clean"
        close_code: Any = None
        try:
            async for raw in self._ws:
                try:
                    msg = json.loads(raw)
                except json.JSONDecodeError:
                    _LOGGER.warning("Invalid JSON from server: %r", raw)
                    continue
                await self._handle_message(msg)
            # Iterator ended without raising = peer closed cleanly (code 1000/1001).
            close_code = getattr(self._ws, "close_code", None)
            _LOGGER.info(
                "WebSocket closed cleanly after %.1fs (code=%s reason=%r)",
                asyncio.get_running_loop().time() - started,
                close_code,
                getattr(self._ws, "close_reason", None),
            )
        except websockets.ConnectionClosed as exc:
            # Close-code + socket-lifetime logging (ledger shq-suite-0019). Kept over the old
            # bland "closed by peer" as a cheap regression signal, but at INFO now the flaps
            # are fixed: they were firmware loop-starvation (the RS485 bridge task starving the
            # ESP's HTTP+WS loop until it went silent, tripping the 30 s availability timeout —
            # fixed by a yield budget in actron-sniffer main.cpp) and came through the
            # coordinator's availability path, not here. This branch fires only when a command
            # hits an already-silent socket ("no close frame received or sent", 1006) or on a
            # genuine close. Bump to WARNING again if you need it visible in /api/error_log.
            kind = "closed"
            close_code = getattr(exc, "code", None) or getattr(self._ws, "close_code", None)
            _LOGGER.info(
                "WebSocket closed after %.1fs: %s: %s",
                asyncio.get_running_loop().time() - started,
                type(exc).__name__,
                exc,
            )
        except Exception as exc:  # noqa: BLE001
            # Anything else that kills the reader is a transport failure, and it used to vanish
            # into the generic reconnect path with no record of what it was (shq-suite-0038).
            kind = "error"
            close_code = type(exc).__name__
            _LOGGER.warning("WebSocket reader failed after %.1fs: %s",
                            asyncio.get_running_loop().time() - started, exc)
        finally:
            if self._on_disconnect is not None:
                lifetime = asyncio.get_running_loop().time() - started
                await self._on_disconnect(kind, close_code, lifetime)

    async def _handle_message(self, msg: dict[str, Any]):
        kind = msg.get("type")
        if self._on_message is not None:
            self._on_message()
        if kind == "state":
            if self._on_state is not None:
                await self._on_state(msg.get("data") or {})
        elif kind == "diag":
            if self._on_diag is not None:
                await self._on_diag(msg.get("event") or {})
        elif kind == "diag_backlog":
            # Replayed history from before this socket existed. Ordered oldest-first by the
            # firmware; the coordinator de-duplicates on `seq`.
            if self._on_diag is not None:
                for event in msg.get("events") or []:
                    await self._on_diag(event)
        elif kind == "health":
            if self._on_health is not None:
                await self._on_health(msg.get("data") or {})
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
