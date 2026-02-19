"""Fault-tolerant API wrapper around ActronAirAPI for Home Assistant."""

import asyncio
import logging
import random
import time

from actron_neo_api import ActronAirAPI, ActronAirAPIError, ActronAirAuthError

_LOGGER = logging.getLogger(__name__)


class ActronAPI:
    """Fault-tolerant wrapper around ActronAirAPI.

    Every SDK call goes through `_call()` which handles retries,
    timeouts, and backoff. Ported from actron-poc/src/api.py with
    click/metrics stripped out.
    """

    BASE_DELAY = 2.0
    MAX_DELAY = 30.0

    def __init__(
        self,
        api: ActronAirAPI,
        timeout: float = 60.0,
        max_retries: int = 3,
    ):
        self._api = api
        self._timeout = timeout
        self._max_retries = max_retries

    async def close(self) -> None:
        """Close the underlying SDK session."""
        await self._api.close()

    async def _call(self, method_name: str, func, *args, **kwargs):
        """Execute an API call with retry, backoff, and timeout."""
        last_error = None

        for attempt in range(1, self._max_retries + 1):
            start = time.monotonic()
            try:
                async with asyncio.timeout(self._timeout):
                    result = await func(*args, **kwargs)

                duration = time.monotonic() - start
                _LOGGER.debug(
                    "[%s] OK in %.2fs (attempt %d)", method_name, duration, attempt
                )
                return result

            except ActronAirAuthError as e:
                duration = time.monotonic() - start
                if attempt == 1:
                    _LOGGER.warning(
                        "[%s] Auth error after %.2fs, refreshing token",
                        method_name, duration,
                    )
                    try:
                        await self._api.oauth2_auth.refresh_access_token()
                    except Exception:
                        pass
                    last_error = e
                    continue
                raise

            except (ActronAirAPIError, asyncio.TimeoutError, Exception) as e:
                duration = time.monotonic() - start
                error_type = type(e).__name__
                last_error = e

                if attempt < self._max_retries:
                    delay = min(
                        self.BASE_DELAY * (2 ** (attempt - 1)) + random.uniform(0, 1),
                        self.MAX_DELAY,
                    )
                    _LOGGER.warning(
                        "[%s] %s: %s (attempt %d/%d, retry in %.1fs)",
                        method_name, error_type, e,
                        attempt, self._max_retries, delay,
                    )
                    await asyncio.sleep(delay)
                else:
                    _LOGGER.error(
                        "[%s] %s: %s (attempt %d/%d, giving up)",
                        method_name, error_type, e,
                        attempt, self._max_retries,
                    )

        raise last_error  # type: ignore[misc]

    # -- Public API methods --------------------------------------------------

    async def get_systems(self) -> list:
        """Fetch all AC systems for this account."""
        return await self._call("get_ac_systems", self._api.get_ac_systems)

    async def get_status(self, serial: str):
        """Fetch current status for a system."""
        await self._call("update_status", self._api.update_status, serial)
        return self._api.state_manager.get_status(serial)

    async def set_mode(self, status, mode: str) -> None:
        """Set system mode (COOL, HEAT, AUTO, FAN, OFF)."""
        await self._call(
            f"set_system_mode({mode})",
            status.user_aircon_settings.set_system_mode,
            mode,
        )

    async def turn_off(self, status) -> None:
        """Turn the system off."""
        await self.set_mode(status, "OFF")

    async def set_temperature(self, status, temp: float) -> None:
        """Set master temperature."""
        await self._call(
            f"set_temperature({temp})",
            status.user_aircon_settings.set_temperature,
            temp,
        )

    async def set_fan_mode(self, status, mode: str) -> None:
        """Set fan mode (HIGH, MEDIUM, LOW, AUTO)."""
        await self._call(
            f"set_fan_mode({mode})",
            status.user_aircon_settings.set_fan_mode,
            mode,
        )

    async def set_continuous_fan(self, status, enabled: bool) -> None:
        """Enable or disable continuous fan mode."""
        await self._call(
            f"set_continuous_mode({enabled})",
            status.user_aircon_settings.set_continuous_mode,
            enabled,
        )

    async def set_away_mode(self, status, enabled: bool) -> None:
        """Enable or disable away mode."""
        await self._call(
            f"set_away_mode({enabled})",
            status.user_aircon_settings.set_away_mode,
            enabled,
        )

    async def set_quiet_mode(self, status, enabled: bool) -> None:
        """Enable or disable quiet mode."""
        await self._call(
            f"set_quiet_mode({enabled})",
            status.user_aircon_settings.set_quiet_mode,
            enabled,
        )

    async def set_turbo_mode(self, status, enabled: bool) -> None:
        """Enable or disable turbo mode."""
        await self._call(
            f"set_turbo_mode({enabled})",
            status.user_aircon_settings.set_turbo_mode,
            enabled,
        )

    async def enable_zone(self, status, zone_index: int, enabled: bool) -> None:
        """Enable or disable a zone."""
        zone = status.remote_zone_info[zone_index]
        await self._call(
            f"zone[{zone_index}].enable({enabled})",
            zone.enable,
            enabled,
        )

    async def set_zone_temperature(
        self, status, zone_index: int, temp: float
    ) -> None:
        """Set a zone's target temperature."""
        zone = status.remote_zone_info[zone_index]
        await self._call(
            f"zone[{zone_index}].set_temperature({temp})",
            zone.set_temperature,
            temp,
        )
