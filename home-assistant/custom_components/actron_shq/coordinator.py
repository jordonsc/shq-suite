"""DataUpdateCoordinator for Actron SHQ integration.

The Actron cloud API takes several seconds to reflect commands in its status
endpoint. To make the UI feel responsive without masking real state, the
coordinator owns:

* An **optimistic overlay** keyed by logical field name. Entities read
  ``get_optimistic(key, fallback)`` so that optimistic values survive across
  unrelated entity updates (concurrent zone toggles, switch flips, etc.).
* A **burst poll window**: after any command, polling accelerates to
  ``BURST_POLL_SECONDS`` for ``BURST_DURATION_SECONDS``. Each command
  resets the window.
* **Change reconciliation**: each overlay records a baseline (the pre-command
  value). On each poll, if the server value matches the desired overlay value
  the overlay is confirmed and cleared; if it still matches the baseline the
  overlay is kept (server hasn't applied yet — and may briefly report stale
  old values even after applying, hence comparing against a fixed baseline
  rather than the previous poll); any other value means an external change
  and the overlay is dropped so the server wins.
* **Rate-limit handling**: on 429/503 from the API, the burst is aborted and
  the coordinator backs off for ``RATE_LIMIT_COOLDOWN_SECONDS``.
"""

import asyncio
import logging
import time
from datetime import timedelta
from typing import Any

from homeassistant.core import HomeAssistant
from homeassistant.helpers.update_coordinator import DataUpdateCoordinator, UpdateFailed

from .api import ActronAPI, ActronRateLimitError
from .const import (
    BASE_POLL_SECONDS,
    BURST_DURATION_SECONDS,
    BURST_POLL_SECONDS,
    RATE_LIMIT_COOLDOWN_SECONDS,
)

_LOGGER = logging.getLogger(__name__)


def _read_status_field(status, key: str) -> Any:
    """Read a logical field from an ``ActronAirStatus`` by overlay key."""
    s = status.user_aircon_settings
    if key == "is_on":
        return s.is_on
    if key == "mode":
        return s.mode
    if key == "fan_mode":
        return s.fan_mode
    if key == "temp_cool":
        return s.temperature_setpoint_cool_c
    if key == "temp_heat":
        return s.temperature_setpoint_heat_c
    if key == "continuous_fan_enabled":
        return s.continuous_fan_enabled
    if key == "away_mode":
        return s.away_mode
    if key == "quiet_mode_enabled":
        return s.quiet_mode_enabled
    if key == "turbo_enabled":
        return s.turbo_enabled
    if key.startswith("zone_"):
        # zone_{idx}_{attr}
        _, idx, attr = key.split("_", 2)
        z = status.remote_zone_info[int(idx)]
        if attr == "active":
            return z.is_active
        if attr == "temp_cool":
            return z.temperature_setpoint_cool_c
        if attr == "temp_heat":
            return z.temperature_setpoint_heat_c
    raise KeyError(key)


class ActronCoordinator(DataUpdateCoordinator):
    """Coordinator to poll Actron Air cloud API."""

    def __init__(self, hass: HomeAssistant, api: ActronAPI) -> None:
        """Initialise the coordinator."""
        self.api = api
        self.serial: str | None = None
        self.command_lock = asyncio.Lock()

        # overlay key -> (desired_value, baseline_value)
        self._optimistic: dict[str, tuple[Any, Any]] = {}
        self._burst_end: float = 0.0
        self._last_command: str | None = None
        self._last_command_at: float = 0.0
        self._rate_limited_until: float = 0.0
        self._timed_out_keys: set[str] = set()
        self._last_error: str | None = None

        super().__init__(
            hass,
            _LOGGER,
            name="Actron SHQ",
            update_interval=timedelta(seconds=BASE_POLL_SECONDS),
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

    # -- Optimistic overlay --------------------------------------------------

    def get_optimistic(self, key: str, fallback: Any) -> Any:
        """Read the desired overlay value if set, otherwise return ``fallback``."""
        entry = self._optimistic.get(key)
        if entry is not None:
            return entry[0]
        return fallback

    def set_optimistic(
        self,
        values: dict[str, Any],
        command_desc: str,
        baselines: dict[str, Any] | None = None,
    ) -> None:
        """Apply overlay values and start/extend the burst window.

        ``values`` maps overlay keys to their desired values. The current
        status value is captured as the baseline so later reconciliation can
        distinguish "not yet applied" from "confirmed" even when the server
        briefly bounces back to stale readings.

        ``baselines`` overrides the baseline for specific keys — needed when
        the caller has already mutated the status object before invoking this
        method (e.g. zone toggles pre-mutate ``enabled_zones`` so concurrent
        commands see the updated list, but that mutation would otherwise
        poison the captured baseline).
        """
        baselines = baselines or {}
        for key, desired in values.items():
            existing = self._optimistic.get(key)
            if key in baselines:
                baseline = baselines[key]
            elif existing is not None:
                # Preserve the original baseline across repeated commands —
                # otherwise we'd capture the previous desired value.
                baseline = existing[1]
            else:
                baseline = self._current_field(key)
            self._optimistic[key] = (desired, baseline)
        self._timed_out_keys.difference_update(values.keys())
        self._last_command = command_desc
        self._last_command_at = time.monotonic()
        self._enter_burst()
        self.async_update_listeners()

    def _current_field(self, key: str) -> Any:
        """Read the current underlying value for a key, or ``None`` if unavailable."""
        if self.data is None:
            return None
        try:
            return _read_status_field(self.data, key)
        except (KeyError, IndexError, AttributeError):
            return None

    def is_pending(self, key: str) -> bool:
        """Return True if this overlay key has an unresolved optimistic value."""
        return key in self._optimistic

    def any_pending(self, keys: list[str]) -> bool:
        """Return True if any of ``keys`` has an unresolved overlay."""
        return any(k in self._optimistic for k in keys)

    def clear_optimistic(self, keys: list[str]) -> None:
        """Remove overlay entries (e.g. on command failure)."""
        for k in keys:
            self._optimistic.pop(k, None)
        self.async_update_listeners()

    # -- Burst polling -------------------------------------------------------

    def _enter_burst(self) -> None:
        """Start or extend the fast-polling window."""
        if self._rate_limited_until > time.monotonic():
            # Respect cooldown — don't hammer while rate limited
            return
        self._burst_end = time.monotonic() + BURST_DURATION_SECONDS
        if self.update_interval != timedelta(seconds=BURST_POLL_SECONDS):
            self.update_interval = timedelta(seconds=BURST_POLL_SECONDS)
        self._reschedule()

    def extend_burst(self) -> None:
        """Push out the fast-poll window without rescheduling the next poll.

        Long-running zone confirmation/hold loops call this each tick so the
        coordinator keeps polling at burst cadence and doesn't prematurely move
        the still-pending overlay into ``timed_out_keys`` (which happens at
        ``_burst_end``). Unlike ``_enter_burst`` it does not cancel/reschedule
        the in-flight refresh, so the 1s polls keep firing on their own timer.
        """
        now = time.monotonic()
        if self._rate_limited_until > now:
            # Respect cooldown — don't fight rate limiting.
            return
        self._burst_end = now + BURST_DURATION_SECONDS
        if self.update_interval != timedelta(seconds=BURST_POLL_SECONDS):
            # Burst wasn't active (e.g. just left cooldown) — start it properly.
            self._enter_burst()

    def _exit_burst(self) -> None:
        """Return to baseline polling."""
        self._burst_end = 0.0
        self.update_interval = timedelta(seconds=BASE_POLL_SECONDS)
        self._reschedule()

    def _reschedule(self) -> None:
        """Cancel the next scheduled refresh and reschedule from now."""
        if self._unsub_refresh:
            self._unsub_refresh()
            self._unsub_refresh = None
        if self.update_interval:
            self._schedule_refresh()

    # -- Diagnostic state ----------------------------------------------------

    @property
    def controller_state(self) -> str:
        """High-level state for the diagnostic sensor."""
        now = time.monotonic()
        if self._rate_limited_until > now:
            return "rate_limited"
        if self._optimistic:
            return "pending"
        if self._timed_out_keys:
            return "timeout"
        return "idle"

    @property
    def controller_state_attributes(self) -> dict[str, Any]:
        """Attributes for the diagnostic sensor."""
        now = time.monotonic()
        attrs: dict[str, Any] = {
            "poll_interval_s": int(self.update_interval.total_seconds())
            if self.update_interval
            else None,
            "pending_keys": sorted(self._optimistic.keys()),
            "timed_out_keys": sorted(self._timed_out_keys),
            "last_command": self._last_command,
        }
        if self._last_command_at:
            attrs["seconds_since_command"] = round(now - self._last_command_at, 1)
        if self._burst_end:
            attrs["burst_remaining_s"] = max(0, round(self._burst_end - now, 1))
        if self._rate_limited_until > now:
            attrs["rate_limit_cooldown_s"] = round(self._rate_limited_until - now, 1)
        if self._last_error:
            attrs["last_error"] = self._last_error
        return attrs

    # -- Poll ----------------------------------------------------------------

    async def _async_update_data(self):
        """Fetch latest status, reconcile overlays, manage burst."""
        if self.serial is None:
            raise UpdateFailed("No serial number — setup incomplete")

        try:
            status = await self.api.get_status(self.serial)
        except ActronRateLimitError as err:
            self._rate_limited_until = time.monotonic() + RATE_LIMIT_COOLDOWN_SECONDS
            self._last_error = f"rate_limited: {err}"
            if self._burst_end:
                # Abort burst — do not retry aggressively. Any pending overlays
                # remain in place; they'll time out or reconcile on recovery.
                self._exit_burst()
            self.async_update_listeners()
            raise UpdateFailed(f"Rate limited: {err}") from err
        except Exception as err:
            self._last_error = f"{type(err).__name__}: {err}"
            raise UpdateFailed(f"Failed to update Actron status: {err}") from err

        self._last_error = None

        # Reconcile each overlay against its baseline captured at command time:
        #   new == desired  -> confirmed, drop overlay
        #   new == baseline -> not applied yet, keep overlay
        #   otherwise       -> external change, drop overlay (server wins)
        # Baseline comparison (not previous-poll comparison) is important
        # because the cloud status endpoint can briefly bounce back to stale
        # values after a change is applied — we don't want to drop the overlay
        # on the first "applied" read only to flicker back on the next.
        for key, (desired, baseline) in list(self._optimistic.items()):
            try:
                new_val = _read_status_field(status, key)
            except (KeyError, IndexError, AttributeError):
                continue
            if new_val == desired:
                self._optimistic.pop(key, None)
                self._timed_out_keys.discard(key)
            elif new_val == baseline:
                continue
            else:
                _LOGGER.debug(
                    "Overlay %s dropped: server reported %r (desired %r, baseline %r)",
                    key, new_val, desired, baseline,
                )
                self._optimistic.pop(key, None)
                self._timed_out_keys.discard(key)

        # Handle burst expiry
        now = time.monotonic()
        if self._burst_end and now >= self._burst_end:
            # Any overlays still set never confirmed within the window.
            if self._optimistic:
                _LOGGER.warning(
                    "Burst window expired with unconfirmed commands: %s",
                    list(self._optimistic.keys()),
                )
                self._timed_out_keys = set(self._optimistic.keys())
                self._optimistic.clear()
            self._exit_burst()

        # Exit rate-limit cooldown if elapsed
        if self._rate_limited_until and now >= self._rate_limited_until:
            self._rate_limited_until = 0.0

        return status
