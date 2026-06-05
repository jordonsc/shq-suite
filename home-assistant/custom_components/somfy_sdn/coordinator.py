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
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator

from .client import SomfySdnClient
from .const import (
    AVAILABILITY_TIMEOUT_S,
    CONF_HOST,
    CONF_PORT,
    DEFAULT_PORT,
    DOMAIN,
    RECONNECT_DELAY_S,
)

_LOGGER = logging.getLogger(__name__)


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
        self.client = SomfySdnClient(self.host, self.port)
        self.client.set_state_callback(self._on_state)
        self.client.set_disconnect_callback(self._on_disconnect)

        self._run_task: Optional[asyncio.Task] = None
        self._reconnect_task: Optional[asyncio.Task] = None
        self._availability_task: Optional[asyncio.Task] = None
        self._shutdown = False
        self._last_msg_time: Optional[float] = None
        self._last_available: bool = False
        self._connecting: bool = False
        # Jog step size (pulses) per motor addr — shared between the Number entity (sets it) and
        # the Jog up/down buttons (read it). Defaults applied by the Number entity.
        self.jog_steps: dict[str, int] = {}

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
            try:
                await self.client.connect()
            except Exception as err:  # noqa: BLE001
                _LOGGER.warning("Connect to %s:%s failed: %s", self.host, self.port, err)
                self._schedule_reconnect()
                return
            _LOGGER.info("Connected to somfy-sdn at %s:%s", self.host, self.port)
            self._run_task = asyncio.create_task(self.client.run())
        finally:
            self._connecting = False

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
        self.async_set_updated_data(data)

    async def _on_disconnect(self):
        _LOGGER.info("Disconnected from somfy-sdn — will reconnect")
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
