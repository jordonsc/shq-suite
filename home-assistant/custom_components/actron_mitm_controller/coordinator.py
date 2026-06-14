"""Coordinator: persistent WS connection + push-driven state, no retry/optimistic logic."""

import asyncio
import logging
import time
from typing import Any, Optional

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator

from .client import ActronMitmClient
from .const import (
    AVAILABILITY_TIMEOUT_S,
    CONF_HOST,
    CONF_PORT,
    DEFAULT_PORT,
    DOMAIN,
    RECONNECT_DELAY_S,
)

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
        self.client.set_disconnect_callback(self._on_disconnect)

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
                self._schedule_reconnect()
                return
            _LOGGER.info("Connected to actron-mitm at %s:%s", self.host, self.port)
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

    async def _on_state(self, data: dict[str, Any]):
        self._last_msg_time = time.time()
        # Push to all subscribed entities. HA's update coordinator handles dispatch.
        self.async_set_updated_data(data)

    async def _on_disconnect(self):
        _LOGGER.info("Disconnected from actron-mitm — will reconnect")
        if not self._shutdown:
            self._schedule_reconnect()

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
