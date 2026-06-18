"""Data update coordinator for the Argus integration."""
import asyncio
import logging
import time
from datetime import timedelta
from typing import Any, Dict, Optional

from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .client import ArgusClient

_LOGGER = logging.getLogger(__name__)

# Grace period before marking the daemon unavailable (seconds)
AVAILABILITY_TIMEOUT = 60


class ArgusCoordinator(DataUpdateCoordinator):
    """Coordinator to manage Argus status and maintain the control WebSocket."""

    def __init__(
        self,
        hass: HomeAssistant,
        name: str,
        host: str,
        port: int = 8770,
    ):
        """Initialize the coordinator."""
        super().__init__(
            hass,
            _LOGGER,
            name=f"Argus {name}",
            update_interval=timedelta(seconds=30),  # Fallback polling interval
        )
        self.friendly_name = name
        self.host = host
        self.port = port
        self.client = ArgusClient(host, port)
        self._listen_task: Optional[asyncio.Task] = None
        self._connected = False
        self._reconnect_task: Optional[asyncio.Task] = None
        self._shutdown = False
        self._last_update_time: Optional[float] = None
        self._availability_task: Optional[asyncio.Task] = None
        self._last_availability_state: bool = False
        self._connecting = False

    async def async_start(self):
        """Start the coordinator and establish the WebSocket connection."""
        # Connect in background to avoid blocking startup
        asyncio.create_task(self._async_connect())
        # Start availability monitoring task
        self._availability_task = asyncio.create_task(self._monitor_availability())

    async def _async_connect(self):
        """Connect to the WebSocket server and start listening."""
        # Prevent concurrent connection attempts
        if self._connected or self._connecting or self._shutdown:
            return

        self._connecting = True
        _LOGGER.info(f"Attempting to connect to Argus at {self.host}:{self.port}")

        connect_success = False
        try:
            if await self.client.connect():
                self._connected = True
                connect_success = True
                # Cancel any pending reconnect task
                if self._reconnect_task and not self._reconnect_task.done():
                    self._reconnect_task.cancel()
                    self._reconnect_task = None
                # Start listening task for push updates (runs in background)
                self._listen_task = asyncio.create_task(
                    self._async_listen_for_updates()
                )
                _LOGGER.info(f"Successfully connected to Argus at {self.host}:{self.port}")
            else:
                _LOGGER.warning(f"Failed to connect to Argus at {self.host}:{self.port}, will retry")
        except Exception as err:
            _LOGGER.error(f"Error connecting to Argus: {err}")
        finally:
            self._connecting = False
            # Schedule reconnect AFTER resetting _connecting flag
            if not connect_success and not self._shutdown:
                self._schedule_reconnect()

    async def _async_listen_for_updates(self):
        """Listen for status broadcasts from the server."""
        try:
            await self.client.start_receiving(self._handle_status_update)
        except Exception as err:
            _LOGGER.error(f"Error in listen task: {err}")
        finally:
            # Connection lost, clean up and schedule reconnect
            self._connected = False
            if not self._shutdown:
                _LOGGER.info("Connection lost, scheduling reconnect...")
                self._schedule_reconnect()

    def _schedule_reconnect(self, delay: int = 5):
        """Schedule a reconnection attempt."""
        if self._shutdown or self._connected or self._connecting:
            return

        # Cancel any existing reconnect task
        if self._reconnect_task and not self._reconnect_task.done():
            _LOGGER.debug("Reconnect already scheduled, skipping")
            return  # Already scheduled

        _LOGGER.info(f"Scheduling reconnect in {delay} seconds")
        self._reconnect_task = asyncio.create_task(self._reconnect_after_delay(delay))

    async def _reconnect_after_delay(self, delay: int):
        """Wait and then attempt to reconnect."""
        try:
            await asyncio.sleep(delay)
            if not self._shutdown:
                # Clear reconnect task before calling _async_connect so it can schedule a new one if it fails
                self._reconnect_task = None
                await self._async_connect()
        except asyncio.CancelledError:
            pass

    @callback
    def _handle_status_update(self, data: Dict[str, Any]):
        """Handle an incoming message from the server."""
        # Track last update time for ANY message — this keeps availability fresh
        # even if no status change has occurred.
        self._last_update_time = time.time()

        if data.get("type") == "status":
            # Update coordinator data with the new status snapshot
            self.async_set_updated_data(data)

    def is_available(self) -> bool:
        """Check if the daemon is available based on last update time."""
        if self._last_update_time is None:
            # No updates received yet, consider unavailable
            return False

        time_since_update = time.time() - self._last_update_time
        return time_since_update < AVAILABILITY_TIMEOUT

    async def _monitor_availability(self):
        """Periodically check availability and trigger updates when it changes."""
        while not self._shutdown:
            try:
                await asyncio.sleep(10)  # Check every 10 seconds

                current_availability = self.is_available()

                # If availability changed, trigger an update to refresh entities
                if current_availability != self._last_availability_state:
                    self._last_availability_state = current_availability
                    if current_availability:
                        _LOGGER.info("Argus became available")
                    else:
                        _LOGGER.warning("Argus became unavailable (no updates for 60+ seconds)")

                    # Trigger coordinator update to refresh entity availability
                    if self.data:
                        self.async_set_updated_data(self.data)

            except asyncio.CancelledError:
                break
            except Exception as err:
                _LOGGER.error(f"Error in availability monitor: {err}")

    async def _async_update_data(self) -> Dict[str, Any]:
        """Fallback polling — Argus is push-only, so just surface cached data."""
        if not self._connected:
            # Try to reconnect
            self._schedule_reconnect(delay=0)
            raise UpdateFailed("Not connected to Argus")

        # Connected via WebSocket — status arrives as broadcasts. Return the
        # cached snapshot to satisfy the polling contract.
        if self.data:
            return self.data

        # Connected but no status yet — return an empty snapshot.
        return {}

    async def async_shutdown(self):
        """Shutdown the coordinator and close connections."""
        self._shutdown = True
        self._connected = False

        # Cancel availability monitor task
        if self._availability_task and not self._availability_task.done():
            self._availability_task.cancel()
            try:
                await self._availability_task
            except asyncio.CancelledError:
                pass

        # Cancel reconnect task
        if self._reconnect_task and not self._reconnect_task.done():
            self._reconnect_task.cancel()
            try:
                await self._reconnect_task
            except asyncio.CancelledError:
                pass

        # Cancel listen task
        if self._listen_task and not self._listen_task.done():
            self._listen_task.cancel()
            try:
                await self._listen_task
            except asyncio.CancelledError:
                pass

        await self.client.disconnect()

    async def async_send_command(self, command_func, *args, **kwargs) -> bool:
        """Send a command to the server."""
        if not self._connected:
            _LOGGER.warning("Not connected, triggering reconnect")
            self._schedule_reconnect(delay=0)
            return False

        try:
            return await command_func(*args, **kwargs)
        except Exception as err:
            _LOGGER.error(f"Error sending command: {err}")
            self._connected = False
            self._schedule_reconnect()
            return False
