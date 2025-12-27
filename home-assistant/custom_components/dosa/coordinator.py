"""Data update coordinator for DOSA integration."""
import asyncio
import logging
import time
from datetime import timedelta
from typing import Any, Dict, Optional

from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .client import DosaClient

_LOGGER = logging.getLogger(__name__)

# Grace period before marking device unavailable (seconds)
AVAILABILITY_TIMEOUT = 30


class DosaCoordinator(DataUpdateCoordinator):
    """Coordinator to manage DOSA data and maintain WebSocket connection."""

    def __init__(
        self,
        hass: HomeAssistant,
        device_id: str,
        name: str,
        host: str,
        port: int = 8766,
    ):
        """Initialize the coordinator."""
        super().__init__(
            hass,
            _LOGGER,
            name=f"DOSA {name}",
            update_interval=timedelta(minutes=5),  # Fallback polling only (WebSocket provides real-time updates)
        )
        self.device_id = device_id
        self.host = host
        self.port = port
        self.client = DosaClient(host, port)
        self._listen_task: Optional[asyncio.Task] = None
        self._connected = False
        self._connecting = False
        self._reconnect_task: Optional[asyncio.Task] = None
        self._shutdown = False
        self._last_update_time: Optional[float] = None
        self._availability_task: Optional[asyncio.Task] = None
        self._last_availability_state: bool = False

    async def async_start(self):
        """Start the coordinator and establish WebSocket connection."""
        # Connect in background to avoid blocking startup
        asyncio.create_task(self._async_connect())
        # Start availability monitoring task
        self._availability_task = asyncio.create_task(self._monitor_availability())

    async def _async_connect(self):
        """Connect to the WebSocket server and start listening."""
        # Prevent concurrent connection attempts
        if self._connected or self._connecting or self._shutdown:
            _LOGGER.warning(
                f"[RECONNECT] Skipping connect: connected={self._connected}, "
                f"connecting={self._connecting}, shutdown={self._shutdown}"
            )
            return

        self._connecting = True
        _LOGGER.warning(f"[RECONNECT] Attempting to connect to DOSA at {self.host}:{self.port}")

        connect_success = False
        try:
            if await self.client.connect():
                self._connected = True
                connect_success = True
                # Cancel any pending reconnect task
                if self._reconnect_task and not self._reconnect_task.done():
                    self._reconnect_task.cancel()
                    self._reconnect_task = None
                # Start listening task for push updates (don't await - runs in background)
                self._listen_task = asyncio.create_task(
                    self._async_listen_for_updates()
                )
                _LOGGER.warning(f"[RECONNECT] Successfully connected to DOSA at {self.host}:{self.port}")
            else:
                _LOGGER.warning(f"[RECONNECT] Failed to connect to DOSA at {self.host}:{self.port}, will retry")
        except Exception as err:
            _LOGGER.warning(f"[RECONNECT] Error connecting to DOSA: {err}")
        finally:
            self._connecting = False
            # Schedule reconnect AFTER resetting _connecting flag
            if not connect_success and not self._shutdown:
                _LOGGER.warning(f"[RECONNECT] Scheduling next reconnect attempt")
                self._schedule_reconnect()

    async def _async_listen_for_updates(self):
        """Listen for status broadcasts from the server."""
        try:
            _LOGGER.warning(f"[RECONNECT] Starting to listen for updates from {self.host}:{self.port}")
            await self.client.start_receiving(self._handle_status_update)
        except Exception as err:
            _LOGGER.warning(f"[RECONNECT] Error in listen task: {err}")
        finally:
            # Connection lost, clean up and schedule reconnect
            _LOGGER.warning(f"[RECONNECT] Listening stopped for {self.host}:{self.port}")
            self._connected = False
            if not self._shutdown:
                _LOGGER.warning("[RECONNECT] Connection lost, scheduling reconnect...")
                self._schedule_reconnect()

    def _schedule_reconnect(self, delay: int = 5):
        """Schedule a reconnection attempt with exponential backoff."""
        if self._shutdown or self._connected or self._connecting:
            _LOGGER.warning(f"[RECONNECT] Not scheduling reconnect: shutdown={self._shutdown}, connected={self._connected}, connecting={self._connecting}")
            return

        # Cancel any existing reconnect task
        if self._reconnect_task and not self._reconnect_task.done():
            _LOGGER.warning("[RECONNECT] Reconnect already scheduled, skipping")
            return  # Already scheduled

        _LOGGER.warning(f"[RECONNECT] Scheduling reconnect in {delay} seconds")
        self._reconnect_task = asyncio.create_task(self._reconnect_after_delay(delay))

    async def _reconnect_after_delay(self, delay: int):
        """Wait and then attempt to reconnect."""
        try:
            await asyncio.sleep(delay)
            if not self._shutdown:
                _LOGGER.warning(f"[RECONNECT] Reconnect delay ({delay}s) complete, calling _async_connect")
                # Clear reconnect task before calling _async_connect so it can schedule a new one if it fails
                self._reconnect_task = None
                await self._async_connect()
            else:
                _LOGGER.warning("[RECONNECT] Reconnect cancelled due to shutdown")
        except asyncio.CancelledError:
            _LOGGER.warning("[RECONNECT] Reconnect task cancelled")
            pass

    @callback
    def _handle_status_update(self, data: Dict[str, Any]):
        """Handle incoming status update from server."""
        # Track last update time for ANY message (including NOOP keepalives)
        # This prevents false unavailable states when device is idle
        self._last_update_time = time.time()

        if data.get('type') == 'status':
            _LOGGER.debug(f"Received status update: {data}")
            # Update coordinator data with new status
            self.async_set_updated_data(data)
            _LOGGER.debug(f"Updated coordinator data, triggering entity updates")

    def is_available(self) -> bool:
        """Check if device is available based on last update time."""
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
                        _LOGGER.info("Device became available")
                    else:
                        _LOGGER.warning("Device became unavailable (no updates for 30+ seconds)")

                    # Trigger coordinator update to refresh entity availability
                    # Use current data if available, otherwise mark as unavailable
                    if self.data:
                        self.async_set_updated_data(self.data)

            except asyncio.CancelledError:
                break
            except Exception as err:
                _LOGGER.error(f"Error in availability monitor: {err}")

    async def _async_update_data(self) -> Dict[str, Any]:
        """Fetch data from API endpoint (fallback polling)."""
        if not self._connected:
            # Don't try to reconnect from polling - let the reconnect task handle it
            # Just schedule one if not already scheduled
            if not self._connecting and not self._reconnect_task:
                self._schedule_reconnect(delay=0)
            raise UpdateFailed("Not connected to DOSA server")

        try:
            status = await self.client.get_status()
            if status:
                # Update last update time on successful poll
                self._last_update_time = time.time()
                return status
            raise UpdateFailed("Failed to get status")
        except Exception as err:
            _LOGGER.error(f"Error fetching data: {err}")
            self._connected = False
            # Only schedule reconnect if not already connecting or scheduled
            if not self._connecting and not self._reconnect_task:
                self._schedule_reconnect()
            raise UpdateFailed(f"Error communicating with API: {err}")

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
            result = await command_func(*args, **kwargs)
            # Don't request refresh - WebSocket broadcasts provide real-time updates
            return result
        except Exception as err:
            _LOGGER.error(f"Error sending command: {err}")
            self._connected = False
            self._schedule_reconnect()
            return False
