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
    "pong_timeout": "the controller evicted it after 45 s with nothing inbound",
    "stall_reap": "the controller's write-guard dropped it for staying unwritable",
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
    parts += _radio_and_tcp(data)
    return ", ".join(parts)


def _radio_and_tcp(data: dict[str, Any]) -> list[str]:
    """The MAC-retry and lwIP-pcb tail shared by socket events (fw 1.12.0).

    `tx_*` are deltas since the previous diag record: the MAC's picture of the run-up. The
    `tcp_*` keys are lwIP's own view of that socket (live at a reap, the last 1 Hz sample at a
    disconnect) — retransmit count and bytes in flight say whether the uplink was getting through.
    """
    parts: list[str] = []
    if data.get("tx_ok") is not None or data.get("tx_retry") is not None:
        parts.append(
            f"MAC since last record: {data.get('tx_ok', 0)} acked / "
            f"{data.get('tx_retry', 0)} retries / {data.get('tx_to', 0)} ACK timeouts"
        )
    if data.get("tcp_state") is not None:
        parts.append(
            f"TCP {data['tcp_state']}, nrtx {data.get('tcp_nrtx', '?')}, "
            f"rto {data.get('tcp_rto_ms', '?')} ms, {data.get('tcp_unacked', '?')} B unacked, "
            f"cwnd {data.get('tcp_cwnd', '?')}"
        )
    return parts


def _describe_stall_reap(data: dict[str, Any]) -> str:
    # The write-guard gave up on a socket that would not accept writes. `value` is how long it
    # had been unwritable; rx=0 with a short lifetime is a socket that was never healthy.
    parts = [
        f"Write-guard reaped client {data.get('client_id', '?')} after "
        f"{_secs(data.get('value'))} unwritable (socket age {_secs(data.get('lifetime_ms'))}, "
        f"{data.get('rx_msgs', '?')} frames received)"
    ]
    parts += _radio_and_tcp(data)
    return ", ".join(parts)


def _describe_evict(data: dict[str, Any]) -> str:
    # The liveness policy (fw 1.14.0) gave up on a client silent in both directions. `value` is
    # the silence; pong age says whether OUR pings were being answered before it went quiet.
    parts = [
        f"Controller evicted client {data.get('client_id', '?')} after "
        f"{_secs(data.get('value'))} with nothing inbound (socket age "
        f"{_secs(data.get('lifetime_ms'))}, last pong {_secs(data.get('pong_age_ms'))} earlier)"
    ]
    parts += _radio_and_tcp(data)
    return ", ".join(parts)


def _describe_ha_closed(data: dict[str, Any]) -> str:
    # `closed_by` comes from the close frames actually seen (rcvd/sent), not the ambiguous
    # legacy `.code`, so this line says who hung up rather than only that someone did.
    who = data.get("closed_by")
    tail = f", closed by {who}" if who else ""
    codes = []
    if data.get("close_rcvd") is not None:
        codes.append(f"rcvd {data['close_rcvd']}")
    if data.get("close_sent") is not None:
        codes.append(f"sent {data['close_sent']}")
    code = ", ".join(codes) if codes else f"code {data.get('close_code')}"
    return (f"Home Assistant lost the socket after {data.get('lifetime_s', '?')} s "
            f"({code}{tail}, {data.get('frames', '?')} frames received)")


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
    "ws_stall_reap": _describe_stall_reap,
    "ws_evict": _describe_evict,
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
    "ha_closed": _describe_ha_closed,
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
