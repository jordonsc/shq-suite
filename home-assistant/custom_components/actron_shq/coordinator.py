"""DataUpdateCoordinator for Actron SHQ integration."""

import logging
from datetime import timedelta

from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .api import ActronAPI
from .const import POLL_INTERVAL_SECONDS

_LOGGER = logging.getLogger(__name__)


class ActronCoordinator(DataUpdateCoordinator):
    """Coordinator to poll Actron Air cloud API."""

    def __init__(self, hass: HomeAssistant, api: ActronAPI) -> None:
        """Initialise the coordinator."""
        self.api = api
        self.serial: str | None = None

        super().__init__(
            hass,
            _LOGGER,
            name="Actron SHQ",
            update_interval=timedelta(seconds=POLL_INTERVAL_SECONDS),
        )

    async def async_setup(self) -> None:
        """Discover AC system serial and perform first poll."""
        try:
            systems = await self.api.get_systems()
        except Exception as err:
            raise UpdateFailed(f"Failed to discover AC systems: {err}") from err

        if not systems:
            raise UpdateFailed("No AC systems found on this account")

        self.serial = systems[0]["serial"]
        _LOGGER.info("Discovered AC system: %s", self.serial)

    def reset_poll_timer(self) -> None:
        """Reset the poll timer to avoid stale fetches during optimistic updates.

        Cancels the current scheduled refresh and reschedules it, pushing
        the next poll out by a full interval from now.
        """
        if self._unsub_refresh:
            self._unsub_refresh()
            self._unsub_refresh = None
        if self.update_interval:
            self._schedule_refresh()

    async def _async_update_data(self):
        """Fetch latest status from the Actron API."""
        if self.serial is None:
            raise UpdateFailed("No serial number — setup incomplete")

        try:
            return await self.api.get_status(self.serial)
        except Exception as err:
            raise UpdateFailed(f"Failed to update Actron status: {err}") from err
