"""Coordinator: persistent WS connection + push-driven state, no retry/optimistic logic."""

import asyncio
import logging
import time
from typing import Any, Optional

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator

from homeassistant.helpers.dispatcher import async_dispatcher_send

from .client import ActronMitmClient
from .const import (
    AVAILABILITY_TIMEOUT_S,
    CONF_HOST,
    CONF_PORT,
    DEFAULT_PORT,
    DIAG_SEEN_MAX,
    DOMAIN,
    EVENT_DIAG,
    RECONNECT_DELAY_S,
    SOURCE_DEVICE,
    SOURCE_HA,
)

# Firmware events that indicate something went wrong, logged at WARNING so they surface in
# /api/error_log without turning on debug. Everything else is INFO.
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

_LOGGER = logging.getLogger(__name__)


class ActronMitmCoordinator(DataUpdateCoordinator[dict[str, Any]]):
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
        self.client = ActronMitmClient(self.host, self.port)
        self.client.set_state_callback(self._on_state)
        self.client.set_diag_callback(self._on_diag)
        self.client.set_health_callback(self._on_health)
        self.client.set_message_callback(self._on_any_message)
        self.client.set_disconnect_callback(self._on_disconnect)

        self.entry_id: str = entry.entry_id
        # Latest firmware vitals, read by the diagnostic sensors. None until the first push.
        self.health: dict[str, Any] | None = None
        # Sequence numbers already turned into bus events. The firmware replays a backlog on every
        # connect, so without this every reconnect would re-fire the same history.
        self._seen_diag_seqs: set[int] = set()
        self._seen_diag_order: list[int] = []
        # Most recent firmware ws_disconnect record, surfaced as a sensor with its evidence
        # attached so the recorder keeps a timeline of CAUSES, not just a disconnect count.
        self.last_disconnect: dict[str, Any] | None = None

        self._run_task: Optional[asyncio.Task] = None
        self._reconnect_task: Optional[asyncio.Task] = None
        self._availability_task: Optional[asyncio.Task] = None
        self._shutdown = False
        self._last_msg_time: Optional[float] = None
        self._last_available: bool = False
        # Guard against parallel connect attempts. The window exists because
        # _reconnect_after clears _reconnect_task BEFORE awaiting _connect_and_run,
        # so during the 5s connect timeout the availability monitor could otherwise
        # spawn a second reconnect task whose connect would race with the first.
        self._connecting: bool = False

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
            # the firmware's WebSocketsServer client slots forever. Repeat a few times and every
            # slot is a live-but-useless zombie, the ESP refuses all new handshakes, and the
            # device looks dead to HA while HTTP stays fine. Closing sends a FIN so the firmware
            # reaps the slot. (_connecting is set, so the cancel-driven _on_disconnect below
            # won't queue a rival reconnect — _schedule_reconnect early-returns.)
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
                self._schedule_reconnect()
                return
            _LOGGER.info("Connected to actron-mitm at %s:%s", self.host, self.port)
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

    def _schedule_reconnect(self, delay: int = RECONNECT_DELAY_S):
        if self._shutdown or self._connecting:
            return
        if self._reconnect_task is not None and not self._reconnect_task.done():
            return
        self._reconnect_task = asyncio.create_task(self._reconnect_after(delay))

    async def _reconnect_after(self, delay: int):
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

        Before this the timer was only reset by a state push, so a device that was talking to us
        perfectly well — heartbeats, diagnostics, health — could still be declared unavailable if
        the state push specifically stalled. Now the availability timeout means what it says.
        """
        self._last_msg_time = time.time()

    async def _on_state(self, data: dict[str, Any]):
        self._last_msg_time = time.time()
        # Push to all subscribed entities. HA's update coordinator handles dispatch.
        self.async_set_updated_data(data)

    async def _on_diag(self, event: dict[str, Any]):
        """One firmware diagnostic record -> one HA bus event.

        Deduplicated on the firmware's `seq`, which never repeats: the same records arrive twice
        by design (live broadcast, then replayed in the backlog to the next client to connect).
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
            "actron-mitm diag: %s%s %s",
            kind,
            f" ({event['reason']})" if event.get("reason") else "",
            {k: v for k, v in event.items() if k not in ("event", "reason")},
        )
        self._fire_diag(SOURCE_DEVICE, kind, event)

    async def _on_health(self, data: dict[str, Any]):
        self.health = data
        # Sensors listen on their own signal rather than the coordinator's update, so a 30 s health
        # push doesn't drag every climate entity through a state write.
        async_dispatcher_send(self.hass, f"{DOMAIN}_health_{self.entry_id}")

    async def _on_disconnect(self, kind: str, close_code: Any, lifetime_s: float):
        _LOGGER.info(
            "Disconnected from actron-mitm after %.1fs (%s, code=%s) — will reconnect",
            lifetime_s, kind, close_code,
        )
        # Our end of the story. The firmware records why IT dropped the socket; this records why
        # WE saw it drop, and the two together settle who hung up on whom (ledger shq-suite-0038).
        self._fire_diag(SOURCE_HA, f"ha_{kind}", {
            "close_code": close_code,
            "lifetime_s": round(lifetime_s, 1),
        })
        if not self._shutdown:
            self._schedule_reconnect()

    @callback
    def _fire_diag(self, source: str, kind: str, payload: dict[str, Any]):
        self.hass.bus.async_fire(EVENT_DIAG, {
            "entry_id": self.entry_id,
            "host": self.host,
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
                    "actron-mitm %s",
                    "available" if avail else "unavailable (no msgs for %ss)" % AVAILABILITY_TIMEOUT_S,
                )
                # Re-publish current data so entities reread their _attr_available
                if self.data is not None:
                    self.async_set_updated_data(self.data)
            # Belt-and-braces: if we're unavailable, kick a reconnect attempt. The
            # WS-level keepalive should already have fired _on_disconnect and queued
            # this, but if something weird happens (network blip with no ping/pong
            # exchange, library bug, etc.) the existing reconnect-task guard makes this
            # idempotent — we won't double up if a reconnect is already pending.
            if not avail:
                self._schedule_reconnect(delay=0)

    # ---- command helpers passed to entities ----------------------------

    async def async_send_command(self, command: str, **extra) -> dict[str, Any]:
        return await self.client.send_command(command, **extra)
