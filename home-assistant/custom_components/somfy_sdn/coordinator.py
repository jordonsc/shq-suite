"""Coordinator: persistent WS connection + push-driven state, no retry/optimistic logic.

Near-verbatim port of the Actron MITM controller coordinator. `data` is the firmware's `state`
payload dict ({mode, devices:[...], errors_recent}); entities read their motor out of it by addr.
"""

import asyncio
import logging
import time
from typing import Any, Optional

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_send
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator

from .client import SomfySdnClient
from .const import (
    AVAILABILITY_TIMEOUT_S,
    CONF_HOST,
    CONF_PORT,
    DEFAULT_PORT,
    DIAG_SEEN_MAX,
    DOMAIN,
    EVENT_DIAG,
    RECONNECT_DELAY_MAX_S,
    RECONNECT_DELAY_MIN_S,
    SOURCE_DEVICE,
    SOURCE_HA,
)

_LOGGER = logging.getLogger(__name__)

# Firmware events that mean something went wrong, logged at WARNING so they reach
# /api/error_log without turning debug on. Everything else is INFO.
_NOTABLE_EVENTS = {
    "ws_disconnect",
    "ws_error",
    "ws_at_cap",
    "loop_stall",
    "heap_low",
    "socket_low",
    "wifi_down",
    "clock_glitch",
    "heartbeat_stall",
}


def _mac_norm(raw: str | None) -> Optional[str]:
    """Normalise a MAC to bare lowercase hex ("404cca512e64"), or None if it isn't one.

    Accepts the zeroconf unique_id form ("404cca512e64") or the firmware's WiFi.macAddress()
    form ("REDACTED-MAC"); returns None for the manual-flow unique_id ("host:port").
    """
    h = (raw or "").replace(":", "").lower()
    if len(h) == 12 and all(c in "0123456789abcdef" for c in h):
        return h
    return None


def _format_mac(raw: str | None) -> Optional[str]:
    """Display form REDACTED-MAC, or None if `raw` isn't a MAC."""
    h = _mac_norm(raw)
    return ":".join(h[i : i + 2] for i in range(0, 12, 2)).upper() if h else None


class SomfySdnCoordinator(DataUpdateCoordinator[dict[str, Any]]):
    """Manages the WS connection and surfaces server-pushed state to entities."""

    def __init__(self, hass: HomeAssistant, entry: ConfigEntry):
        super().__init__(
            hass,
            _LOGGER,
            name=f"{DOMAIN} {entry.data[CONF_HOST]}",
            update_interval=None,  # push-only; no polling
        )
        self.host: str = entry.data[CONF_HOST]
        self.port: int = entry.data.get(CONF_PORT, DEFAULT_PORT)
        # Stable controller MAC for the device name (IP-independent). Seed from the entry's
        # unique_id (the zeroconf MAC) so the name is right before the WS connects; the live
        # `state` payload refreshes it (and supplies it for manually-added entries).
        self.mac: Optional[str] = _format_mac(entry.unique_id)
        # Stable key for entity unique_ids / device identifiers — the MAC (bare hex) when known,
        # else host:port (manual entries). Fixed for the entry's life so identifiers survive a
        # DHCP change. Migrated from the old host:port scheme by async_migrate_entry.
        self.controller_key: str = _mac_norm(entry.unique_id) or f"{self.host}:{self.port}"
        self.client = SomfySdnClient(self.host, self.port)
        self.client.set_state_callback(self._on_state)
        self.client.set_diag_callback(self._on_diag)
        self.client.set_health_callback(self._on_health)
        self.client.set_message_callback(self._on_any_message)
        self.client.set_disconnect_callback(self._on_disconnect)

        self._run_task: Optional[asyncio.Task] = None
        self._reconnect_task: Optional[asyncio.Task] = None
        self._availability_task: Optional[asyncio.Task] = None
        self._shutdown = False
        self._last_msg_time: Optional[float] = None
        self._last_available: bool = False
        # Reconnect backoff state (const.py): the next delay, and whether the current session
        # has delivered anything — a session that has is a success, whatever ends it.
        self._backoff_s: float = RECONNECT_DELAY_MIN_S
        self._session_frames: int = 0
        self._connecting: bool = False
        # Jog step size (pulses) per motor addr — shared between the Number entity (sets it) and
        # the Jog up/down buttons (read it). Defaults applied by the Number entity.
        self.jog_steps: dict[str, int] = {}

        self.entry_id: str = entry.entry_id
        # Latest firmware vitals, read by the diagnostic sensors. None until the first push.
        self.health: dict[str, Any] | None = None
        # Most recent firmware ws_disconnect record, surfaced as a sensor with its evidence
        # attached so the recorder keeps a timeline of CAUSES, not just a disconnect count.
        self.last_disconnect: dict[str, Any] | None = None
        # Sequences already turned into bus events — the firmware replays a backlog on every
        # connect, so without this each reconnect would re-fire the same history.
        self._seen_diag_seqs: set[int] = set()
        self._seen_diag_order: list[int] = []

    # ---- lifecycle -------------------------------------------------------

    async def async_start(self):
        asyncio.create_task(self._connect_and_run())
        self._availability_task = asyncio.create_task(self._monitor_availability())

    async def async_shutdown(self):
        self._shutdown = True
        for task in (self._run_task, self._reconnect_task, self._availability_task):
            if task is not None and not task.done():
                task.cancel()
                try:
                    await task
                except (asyncio.CancelledError, Exception):  # noqa: BLE001
                    pass
        await self.client.close()

    async def _connect_and_run(self):
        if self._shutdown or self._connecting:
            return
        self._connecting = True
        try:
            # Tear down any previous connection before opening a new one. The availability
            # monitor force-reconnects when state goes quiet for AVAILABILITY_TIMEOUT_S, which
            # can fire while the old socket is still physically alive (e.g. the ESP went silent
            # for >30 s but the TCP stayed half-open). Without an explicit close here, connect()
            # would orphan that socket: it stays alive on its own WS keepalive and holds one of
            # the firmware's 5 WebSocketsServer slots forever. Repeat a few times and every slot
            # is a live-but-useless zombie, the ESP refuses all new handshakes, and the device
            # looks dead to HA while HTTP/the SDN bus stay fine. Closing sends a FIN so the
            # firmware reaps the slot. (_connecting is set, so the cancel-driven _on_disconnect
            # below won't queue a rival reconnect — _schedule_reconnect early-returns.)
            await self._teardown_connection()
            try:
                await self.client.connect()
            except Exception as err:  # noqa: BLE001
                _LOGGER.warning("Connect to %s:%s failed: %s", self.host, self.port, err)
                # A refused/timed-out connect is itself a finding: it is what socket-pool
                # exhaustion on the device looks like from here, as distinct from an established
                # socket going quiet.
                self._fire_diag(SOURCE_HA, "ha_connect_failed", {
                    "error": f"{type(err).__name__}: {err}",
                })
                self._schedule_reconnect(self._bump_backoff())
                return
            _LOGGER.info("Connected to somfy-sdn at %s:%s", self.host, self.port)
            # A fresh socket gets the full AVAILABILITY_TIMEOUT_S to deliver its first frame.
            # Without this the monitor judged a <1 s-old socket on the PREVIOUS session's last
            # message and tore it down before the connect-time snapshot could arrive.
            self._last_msg_time = time.time()
            self._session_frames = 0
            self._fire_diag(SOURCE_HA, "ha_connected", {})
            self._run_task = asyncio.create_task(self.client.run())
        finally:
            self._connecting = False

    async def _teardown_connection(self):
        """Cancel the reader task and close the socket so the firmware reaps our WS slot."""
        task = self._run_task
        self._run_task = None
        if task is not None and not task.done():
            task.cancel()
            try:
                await task
            except (asyncio.CancelledError, Exception):  # noqa: BLE001
                pass
        await self.client.close()

    def _bump_backoff(self) -> float:
        """The delay for the next attempt after a failure, doubling towards the cap."""
        delay = self._backoff_s
        self._backoff_s = min(self._backoff_s * 2, RECONNECT_DELAY_MAX_S)
        return delay

    def _schedule_reconnect(self, delay: float | None = None):
        if delay is None:
            delay = self._backoff_s
        if self._shutdown or self._connecting:
            return
        if self._reconnect_task is not None and not self._reconnect_task.done():
            return
        self._reconnect_task = asyncio.create_task(self._reconnect_after(delay))

    async def _reconnect_after(self, delay: float):
        try:
            await asyncio.sleep(delay)
        except asyncio.CancelledError:
            return
        self._reconnect_task = None
        if not self._shutdown:
            await self._connect_and_run()

    # ---- callbacks from client ------------------------------------------

    @callback
    def _on_any_message(self):
        """Liveness stamp for EVERY inbound frame, not just state.

        Before this only a state push reset the timer, so a device talking to us perfectly well —
        heartbeats, diagnostics, health — could still be declared unavailable if the state push
        specifically stalled.
        """
        self._last_msg_time = time.time()
        self._session_frames += 1

    async def _on_state(self, data: dict[str, Any]):
        self._last_msg_time = time.time()
        mac = _format_mac(data.get("mac"))
        if mac:
            self.mac = mac
        self.async_set_updated_data(data)

    async def _on_diag(self, event: dict[str, Any]):
        """One firmware diagnostic record -> one HA bus event.

        Deduplicated on the firmware's `seq`, which never repeats: the same records arrive twice by
        design (live broadcast, then replayed in the backlog to the next client to connect).
        """
        seq = event.get("seq")
        if isinstance(seq, int):
            if seq in self._seen_diag_seqs:
                return
            self._seen_diag_seqs.add(seq)
            self._seen_diag_order.append(seq)
            if len(self._seen_diag_order) > DIAG_SEEN_MAX:
                self._seen_diag_seqs.discard(self._seen_diag_order.pop(0))

        kind = event.get("event", "unknown")
        if kind == "ws_disconnect":
            self.last_disconnect = {
                # "unclassified" not "unknown": HA renders the literal string `unknown` as the
                # no-data state, which would make a firmware verdict of "no evidence" look
                # identical to never having heard from the device at all.
                "reason": event.get("reason") or "unclassified",
                **{k: v for k, v in event.items() if k not in ("event", "reason")},
            }
            async_dispatcher_send(self.hass, f"{DOMAIN}_disconnect_{self.entry_id}")

        _LOGGER.log(
            logging.WARNING if kind in _NOTABLE_EVENTS else logging.INFO,
            "somfy-sdn %s diag: %s%s %s",
            self.mac or self.host,
            kind,
            f" ({event['reason']})" if event.get("reason") else "",
            {k: v for k, v in event.items() if k not in ("event", "reason")},
        )
        self._fire_diag(SOURCE_DEVICE, kind, event)

    async def _on_health(self, data: dict[str, Any]):
        # Merge, don't replace: fw >= 1.14.0 delivers the vitals as three frames (`health`,
        # `health_ws`, `health_net`) a second apart so none exceeds its 600 B frame budget; every
        # sensor keeps reading the one dict it always did.
        self.health = {**(self.health or {}), **data}
        # Sensors listen on their own signal rather than the coordinator's update, so a 30 s health
        # push doesn't drag every cover entity through a state write.
        async_dispatcher_send(self.hass, f"{DOMAIN}_health_{self.entry_id}")

    async def _on_disconnect(self, kind: str, close_info: dict[str, Any], lifetime_s: float):
        # A session that delivered anything was a success: the next attempt is immediate-ish.
        # One that died before its first frame counts as a failed attempt and backs off.
        if self._session_frames > 0:
            self._backoff_s = RECONNECT_DELAY_MIN_S
            delay = RECONNECT_DELAY_MIN_S
        else:
            delay = self._bump_backoff()
        _LOGGER.info(
            "Disconnected from somfy-sdn after %.1fs (%s, code=%s, closed by %s) — reconnect in %ss",
            lifetime_s, kind, close_info.get("close_code"), close_info.get("closed_by"), delay,
        )
        # Our end of the story, against the firmware's own verdict for the same socket.
        self._fire_diag(SOURCE_HA, f"ha_{kind}", {
            **close_info,
            "lifetime_s": round(lifetime_s, 1),
            "frames": self._session_frames,
        })
        if not self._shutdown:
            self._schedule_reconnect(delay)

    @callback
    def _fire_diag(self, source: str, kind: str, payload: dict[str, Any]):
        self.hass.bus.async_fire(EVENT_DIAG, {
            "entry_id": self.entry_id,
            "host": self.host,
            "mac": self.mac,
            "source": source,
            "event": kind,
            **{k: v for k, v in payload.items() if k != "event"},
        })

    # ---- availability --------------------------------------------------

    @callback
    def is_available(self) -> bool:
        if self._last_msg_time is None:
            return False
        return (time.time() - self._last_msg_time) < AVAILABILITY_TIMEOUT_S

    async def _monitor_availability(self):
        while not self._shutdown:
            try:
                await asyncio.sleep(10)
            except asyncio.CancelledError:
                return
            avail = self.is_available()
            if avail != self._last_available:
                self._last_available = avail
                _LOGGER.log(
                    logging.INFO if avail else logging.WARNING,
                    "somfy-sdn %s",
                    "available" if avail else "unavailable (no msgs for %ss)" % AVAILABILITY_TIMEOUT_S,
                )
                if self.data is not None:
                    self.async_set_updated_data(self.data)
            if not avail:
                self._schedule_reconnect(delay=0)

    # ---- command helper passed to entities -----------------------------

    async def async_send_command(self, command: str, **extra) -> dict[str, Any]:
        return await self.client.send_command(command, **extra)
