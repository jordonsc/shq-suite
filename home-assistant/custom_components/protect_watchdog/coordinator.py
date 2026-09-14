"""Poll the Protect event websocket and decide whether it has gone silent."""

from __future__ import annotations

import logging
import time
from datetime import timedelta

from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator

from .const import DOMAIN
from .probe import ProbeResult, probe

_LOGGER = logging.getLogger(__name__)


class ProtectWatchdogCoordinator(DataUpdateCoordinator[ProbeResult]):
    """Measures socket silence and converts it into a single stale/not-stale call."""

    def __init__(
        self,
        hass: HomeAssistant,
        nvr_host: str,
        nvr_port: int,
        scan_interval: int,
        stale_after: int,
    ) -> None:
        """Initialise the coordinator."""
        super().__init__(
            hass,
            _LOGGER,
            name=DOMAIN,
            update_interval=timedelta(seconds=scan_interval),
            # This is a YAML component with no config entry. Passing None
            # explicitly is the semantically correct value and keeps us off the
            # ContextVar path HA deprecated in 2026.8 (currently IGNOREd for
            # custom integrations, but that will not last).
            config_entry=None,
        )
        self.nvr_host = nvr_host
        self.nvr_port = nvr_port
        self.stale_after = stale_after
        self._silence = 0.0
        self._last_socket_seen = time.monotonic()
        self._was_stale = False

    async def _async_update_data(self) -> ProbeResult:
        """Take one measurement."""
        result = await self.hass.async_add_executor_job(
            probe, self.nvr_host, self.nvr_port
        )

        # The socket's own last-received timer IS the silence: tcpi_last_data_recv
        # already counts from the last byte, so there is nothing to accumulate on
        # top of it. An earlier version tracked "time since a reading below the
        # threshold" instead, which reset itself on every sub-threshold reading
        # and so could only trip after 2x stale_after — caught by fault injection
        # on the live socket, where age climbed to 345s with the stale flag still
        # clear. Do not reintroduce a second clock here.
        if result.age is not None:
            self._silence = result.age
            self._last_socket_seen = time.monotonic()
        else:
            # No socket at all is a failure too, and one the socket cannot time
            # for us: fall back to wall time since we last saw one. This also
            # debounces an HA restart or a config-entry reload, which briefly
            # leave us with no connection.
            self._silence = time.monotonic() - self._last_socket_seen

        stale = self.is_stale
        if stale != self._was_stale:
            self._was_stale = stale
            if stale:
                _LOGGER.error(
                    "UniFi Protect event websocket has been silent for %.0fs "
                    "(%d socket(s) to %s). Detection sensors are frozen; the cure "
                    "is to reload the unifiprotect config entry",
                    self._silence,
                    result.sockets,
                    self.nvr_host,
                )
            else:
                _LOGGER.warning("UniFi Protect event websocket is delivering again")
        return result

    @property
    def seconds_since_healthy(self) -> float:
        """Seconds since the event stream last delivered a byte."""
        return self._silence

    @property
    def is_stale(self) -> bool:
        """Whether the event stream should be considered dead."""
        return self._silence > self.stale_after
