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


def _close_info(exc: Any, ws: Any) -> dict[str, Any]:
    """Who closed, from the legacy-API exception's `rcvd`/`sent` (websockets >= 13).

    `exc.code` is ambiguous: it is 1006 whenever no close frame was seen in EITHER direction,
    which is what our own 20 s pong timeout produces, and it says nothing about who initiated a
    clean close. `rcvd` is the close frame the peer sent, `sent` the one we sent; `rcvd_then_sent`
    orders them (ledger shq-suite-0046, HA-side agent).
    """
    rcvd = getattr(exc, "rcvd", None)
    sent = getattr(exc, "sent", None)
    rcvd_code = getattr(rcvd, "code", None)
    sent_code = getattr(sent, "code", None)
    if rcvd is None and sent is None:
        closed_by = "nobody"  # transport died: our pong timeout, or a RST
    elif rcvd is not None and (sent is None or getattr(exc, "rcvd_then_sent", None)):
        closed_by = "device"
    else:
        closed_by = "ha"
    # The library's own definition of the legacy `.code` (deprecated since websockets 13, and a
    # DeprecationWarning on every access in 15.x): the received close code, else 1006.
    code = rcvd_code if rcvd_code is not None else 1006
    return {
        "close_code": code,
        "close_rcvd": rcvd_code,
        "close_sent": sent_code,
        "closed_by": closed_by,
    }


class SomfySdnClient:
    """Minimal WS client. No reconnect logic — that's the coordinator's job."""

    def __init__(self, host: str, port: int):
        self.host = host
        self.port = port
        self.uri = f"ws://{host}:{port}"
        self._ws: Optional[websockets.WebSocketClientProtocol] = None
        self._pending: dict[str, asyncio.Future] = {}
        self._on_state: Optional[StateCallback] = None
        self._on_diag: Optional[Callable[[dict[str, Any]], Awaitable[None]]] = None
        self._on_health: Optional[Callable[[dict[str, Any]], Awaitable[None]]] = None
        self._on_message: Optional[Callable[[], None]] = None
        self._on_disconnect: Optional[Callable[[str, Any, float], Awaitable[None]]] = None

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

    def set_diag_callback(self, cb):
        self._on_diag = cb

    def set_health_callback(self, cb):
        self._on_health = cb

    def set_message_callback(self, cb):
        """Called for EVERY inbound frame, whatever its type.

        Availability keys off "have we heard anything at all", not "have we had state". A `health`
        or `diag` frame proves the socket is alive just as well as a state push does (ledger
        shq-suite-0038).
        """
        self._on_message = cb

    def set_disconnect_callback(self, cb):
        """cb(kind, close_info, lifetime_s) — `kind` is one of clean/closed/error.

        `close_info` is a dict: `close_code` (the legacy `.code`), `close_rcvd` / `close_sent`
        (the code in the close frame the device sent / we sent, None if none), `closed_by`
        ("device" / "ha" / "nobody" — the last meaning no close frame crossed either way, i.e.
        our pong timeout or a reset), or, for `error`, the exception class under `close_code`.
        """
        self._on_disconnect = cb

    async def run(self):
        """Drain incoming messages until the socket closes."""
        if self._ws is None:
            return
        started = asyncio.get_running_loop().time()
        kind = "clean"
        close_info: dict[str, Any] = {}
        try:
            async for raw in self._ws:
                try:
                    msg = json.loads(raw)
                except json.JSONDecodeError:
                    _LOGGER.warning("Invalid JSON from server: %r", raw)
                    continue
                await self._handle_message(msg)
            close_info = {"close_code": getattr(self._ws, "close_code", None),
                          "closed_by": "device"}
        except websockets.ConnectionClosed as exc:
            # Who closed, and how long the socket lived, are the HA half of the story; the
            # firmware records its own verdict and the two together settle who hung up on whom.
            kind = "closed"
            close_info = _close_info(exc, self._ws)
            _LOGGER.info("WebSocket closed after %.1fs: rcvd=%s sent=%s (closed by %s): %s",
                         asyncio.get_running_loop().time() - started,
                         close_info["close_rcvd"], close_info["close_sent"],
                         close_info["closed_by"], exc)
        except Exception as exc:  # noqa: BLE001
            kind = "error"
            close_info = {"close_code": type(exc).__name__, "closed_by": "exception"}
            _LOGGER.warning("WebSocket reader failed after %.1fs: %s",
                            asyncio.get_running_loop().time() - started, exc)
        finally:
            if self._on_disconnect is not None:
                await self._on_disconnect(
                    kind, close_info, asyncio.get_running_loop().time() - started
                )

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
            # Replayed history from before this socket existed; the coordinator dedupes on `seq`.
            if self._on_diag is not None:
                for event in msg.get("events") or []:
                    await self._on_diag(event)
        elif kind in ("health", "health_ws", "health_net"):
            # fw >= 1.14.0 splits the vitals into three frames so none exceeds its 600 B frame
            # budget (ledger shq-suite-0046); the coordinator merges them. Older firmware sends
            # the whole thing as `health`, which merges just the same.
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
