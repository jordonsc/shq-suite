"""Logbook rendering for `actron_mitm_diag` events.

The bus events carry the full record; this turns each one into a sentence, so the logbook reads as
a narrative of why the link dropped rather than a wall of JSON. The interesting columns are inlined
per event type — a disconnect wants its reason and the loop stall that preceded it, a loop stall
wants its phase attribution, and nothing else needs either.
"""

from __future__ import annotations

from typing import Any, Callable

from homeassistant.components.logbook import LOGBOOK_ENTRY_MESSAGE, LOGBOOK_ENTRY_NAME
from homeassistant.core import Event, HomeAssistant, callback

from .const import DOMAIN, EVENT_DIAG

# How each disconnect classification reads in plain English. The firmware is deliberately
# conservative about these — "unclassified" means it had no evidence, not that nothing happened.
_REASONS = {
    "pong_timeout": "the controller evicted it for a missed keepalive",
    "peer_close": "Home Assistant closed it",
    "transport_error": "a transport error killed it",
    "unclassified": "cause unclassified — no evidence either way",
}


def _describe_disconnect(data: dict[str, Any]) -> str:
    reason = _REASONS.get(data.get("reason", ""), data.get("reason", "cause unknown"))
    parts = [f"WebSocket dropped after {_secs(data.get('lifetime_ms'))} — {reason}"]
    if data.get("pong_age_ms") is not None:
        parts.append(f"last pong {_secs(data['pong_age_ms'])} earlier")
    if data.get("loop_max_ms"):
        parts.append(f"worst loop stall {data['loop_max_ms']} ms")
    if data.get("spare_sockets") is not None:
        parts.append(f"{data['spare_sockets']} spare sockets")
    return ", ".join(parts)


def _describe_loop_stall(data: dict[str, Any]) -> str:
    # Attribution is the whole point: which phase held the loop decides whether this is an HTTP
    # problem, an OTA problem, or the WS layer's own doing.
    phases = {"HTTP": data.get("http_ms"), "OTA": data.get("ota_ms"), "WS": data.get("ws_ms")}
    named = [f"{k} {v} ms" for k, v in phases.items() if v]
    tail = f" ({', '.join(named)})" if named else ""
    return f"Main loop stalled for {data.get('value', '?')} ms{tail}"


def _secs(ms: Any) -> str:
    if not isinstance(ms, (int, float)):
        return "an unknown time"
    return f"{ms / 1000:.1f} s" if ms >= 1000 else f"{int(ms)} ms"


_DESCRIBERS: dict[str, Callable[[dict[str, Any]], str]] = {
    "boot": lambda d: f"Controller booted (reset reason {d.get('value', '?')})",
    "ws_connect": lambda d: f"WebSocket client {d.get('ip', '?')} connected"
                            f" ({d.get('clients', '?')} now connected)",
    "ws_disconnect": _describe_disconnect,
    "ws_error": lambda d: f"WebSocket transport error on client {d.get('client_id', '?')}",
    "ws_at_cap": lambda d: "All WebSocket slots occupied — new connections will be refused",
    "loop_stall": _describe_loop_stall,
    "heap_low": lambda d: f"Free heap fell to {d.get('value', '?')} bytes",
    "socket_low": lambda d: f"Only {d.get('value', '?')} spare sockets left",
    "wifi_down": lambda d: "WiFi association lost",
    "wifi_up": lambda d: f"WiFi reassociated at {d.get('value', '?')} dBm",
    "clock_glitch": lambda d: f"Clock filter caught a bad millis() read ({d.get('value')} total)",
    "heartbeat_stall": lambda d: f"State heartbeat stalled — {_secs(d.get('value'))} since the"
                                 " last push",
    # Our own end of the connection, so the two sides can be read against each other.
    "ha_connected": lambda d: "Home Assistant connected",
    "ha_connect_failed": lambda d: f"Home Assistant could not connect: {d.get('error', '?')}",
    "ha_clean": lambda d: f"Home Assistant saw a clean close after {d.get('lifetime_s', '?')} s",
    "ha_closed": lambda d: f"Home Assistant lost the socket after {d.get('lifetime_s', '?')} s"
                           f" (code {d.get('close_code')})",
    "ha_error": lambda d: f"Home Assistant's reader failed after {d.get('lifetime_s', '?')} s"
                          f" ({d.get('close_code')})",
}


@callback
def async_describe_events(hass: HomeAssistant, async_describe_event) -> None:
    @callback
    def async_describe_diag(event: Event) -> dict[str, str]:
        data = dict(event.data)
        kind = data.get("event", "unknown")
        describer = _DESCRIBERS.get(kind)
        message = describer(data) if describer else f"{kind}: {data}"
        return {
            LOGBOOK_ENTRY_NAME: f"Actron MITM ({data.get('host', 'controller')})",
            LOGBOOK_ENTRY_MESSAGE: message,
        }

    async_describe_event(DOMAIN, EVENT_DIAG, async_describe_diag)
