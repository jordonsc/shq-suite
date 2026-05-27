"""Climate platform for Actron SHQ integration."""

import asyncio
import logging
import time

from homeassistant.components.climate import (
    ClimateEntity,
    ClimateEntityFeature,
    HVACMode,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import ATTR_TEMPERATURE, UnitOfTemperature
from homeassistant.core import HomeAssistant
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .api import ActronRateLimitError
from .const import (
    DOMAIN,
    LAST_ZONE_OFF_HOLD_SECONDS,
    ZONE_CONFIRM_POLL_SECONDS,
    ZONE_CONFIRM_TIMEOUT_SECONDS,
    ZONE_RESEND_INTERVAL_SECONDS,
)
from .coordinator import ActronCoordinator

_LOGGER = logging.getLogger(__name__)

HA_TO_SDK_MODE = {
    HVACMode.OFF: "OFF",
    HVACMode.COOL: "COOL",
    HVACMode.HEAT: "HEAT",
    HVACMode.AUTO: "AUTO",
    HVACMode.FAN_ONLY: "FAN",
}

SDK_TO_HA_MODE = {v: k for k, v in HA_TO_SDK_MODE.items()}

SDK_TO_HA_FAN = {
    "HIGH": "high",
    "MEDIUM": "medium",
    "LOW": "low",
    "AUTO": "auto",
}

HA_TO_SDK_FAN = {v: k for k, v in SDK_TO_HA_FAN.items()}


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Set up Actron climate entities from a config entry."""
    coordinator: ActronCoordinator = hass.data[DOMAIN][entry.entry_id]
    status = coordinator.data

    entities: list[ClimateEntity] = [ActronClimate(coordinator)]

    zones = status.remote_zone_info
    if zones:
        for i, zone in enumerate(zones):
            entities.append(ActronZoneClimate(coordinator, i, zone))

    async_add_entities(entities)


class ActronClimateBase(CoordinatorEntity, ClimateEntity):
    """Base class with per-slot command cancellation.

    Rapid UI adjustments (e.g. dragging a temperature slider) produce multiple
    commands for the same slot. Only the most recent should win — in-flight
    predecessors are cancelled so their retry sleeps and API waits abort.
    Optimistic state lives in the coordinator's overlay, not on the entity,
    so concurrent commands on different entities don't interfere.
    """

    _attr_temperature_unit = UnitOfTemperature.CELSIUS

    def __init__(self, coordinator: ActronCoordinator) -> None:
        """Initialise base climate entity."""
        super().__init__(coordinator)
        self._pending_commands: dict[str, asyncio.Task] = {}

    async def _run_command(
        self,
        slot: str,
        overlay: dict,
        command_desc: str,
        coro,
        baselines: dict | None = None,
    ) -> None:
        """Apply optimistic overlay, run the API call, reconcile on failure."""
        self.coordinator.set_optimistic(overlay, command_desc, baselines=baselines)

        pending = self._pending_commands.get(slot)
        if pending is not None and not pending.done():
            pending.cancel()

        current = asyncio.current_task()
        self._pending_commands[slot] = current

        try:
            async with self.coordinator.command_lock:
                await coro
        except asyncio.CancelledError:
            if self._pending_commands.get(slot) is not current:
                _LOGGER.debug("Command '%s' superseded by newer request", slot)
                return
            raise
        except Exception:
            self.coordinator.clear_optimistic(list(overlay.keys()))
            raise
        finally:
            if self._pending_commands.get(slot) is current:
                self._pending_commands.pop(slot, None)


class ActronClimate(ActronClimateBase):
    """Climate entity for the main Actron AC unit."""

    _attr_supported_features = (
        ClimateEntityFeature.TARGET_TEMPERATURE | ClimateEntityFeature.FAN_MODE
    )
    _attr_hvac_modes = [
        HVACMode.OFF,
        HVACMode.COOL,
        HVACMode.HEAT,
        HVACMode.AUTO,
        HVACMode.FAN_ONLY,
    ]
    _attr_fan_modes = ["low", "medium", "high", "auto"]

    def __init__(self, coordinator: ActronCoordinator) -> None:
        """Initialise the main climate entity."""
        super().__init__(coordinator)
        self._attr_unique_id = f"{DOMAIN}_{coordinator.serial}"
        self._attr_name = "Actron Air Conditioner"

    @property
    def _status(self):
        return self.coordinator.data

    @property
    def _settings(self):
        return self._status.user_aircon_settings

    @property
    def hvac_mode(self) -> HVACMode:
        """Return current HVAC mode (overlay-aware)."""
        is_on = self.coordinator.get_optimistic("is_on", self._settings.is_on)
        if not is_on:
            return HVACMode.OFF
        sdk_mode = self.coordinator.get_optimistic("mode", self._settings.mode)
        return SDK_TO_HA_MODE.get(sdk_mode, HVACMode.OFF)

    @property
    def current_temperature(self) -> float | None:
        """Return average temperature across all active zones."""
        zones = self._status.remote_zone_info
        if not zones:
            return None
        temps = [
            z.live_temp_c
            for z in zones
            if getattr(z, "live_temp_c", None) is not None
        ]
        if not temps:
            return None
        return round(sum(temps) / len(temps), 1)

    @property
    def target_temperature(self) -> float | None:
        """Return the target temperature based on current mode."""
        mode = self.hvac_mode
        if mode == HVACMode.HEAT:
            return self.coordinator.get_optimistic(
                "temp_heat", self._settings.temperature_setpoint_heat_c
            )
        return self.coordinator.get_optimistic(
            "temp_cool", self._settings.temperature_setpoint_cool_c
        )

    @property
    def fan_mode(self) -> str | None:
        """Return current fan mode (overlay-aware)."""
        sdk_fan = self.coordinator.get_optimistic("fan_mode", self._settings.fan_mode)
        return SDK_TO_HA_FAN.get(sdk_fan, None)

    async def async_set_hvac_mode(self, hvac_mode: HVACMode) -> None:
        """Set HVAC mode."""
        sdk_mode = HA_TO_SDK_MODE.get(hvac_mode)
        if sdk_mode is None:
            return
        overlay: dict = {"is_on": sdk_mode != "OFF"}
        if sdk_mode != "OFF":
            overlay["mode"] = sdk_mode
        await self._run_command(
            "mode",
            overlay,
            f"set_mode({sdk_mode})",
            self.coordinator.api.set_mode(self._status, sdk_mode),
        )

    async def async_set_temperature(self, **kwargs) -> None:
        """Set target temperature."""
        temp = kwargs.get(ATTR_TEMPERATURE)
        if temp is None:
            return
        key = "temp_heat" if self.hvac_mode == HVACMode.HEAT else "temp_cool"
        await self._run_command(
            "temperature",
            {key: temp},
            f"set_temperature({temp})",
            self.coordinator.api.set_temperature(self._status, temp),
        )

    async def async_set_fan_mode(self, fan_mode: str) -> None:
        """Set fan mode."""
        sdk_fan = HA_TO_SDK_FAN.get(fan_mode)
        if sdk_fan is None:
            return
        await self._run_command(
            "fan_mode",
            {"fan_mode": sdk_fan},
            f"set_fan_mode({sdk_fan})",
            self.coordinator.api.set_fan_mode(self._status, sdk_fan),
        )

    @property
    def extra_state_attributes(self) -> dict:
        """Expose a `pending` flag when any command is unconfirmed."""
        return {
            "pending": self.coordinator.any_pending(
                ["is_on", "mode", "fan_mode", "temp_cool", "temp_heat"]
            )
        }


class ActronZoneClimate(ActronClimateBase):
    """Climate entity for an Actron AC zone."""

    _attr_supported_features = (
        ClimateEntityFeature.TARGET_TEMPERATURE
        | ClimateEntityFeature.TURN_ON
        | ClimateEntityFeature.TURN_OFF
    )

    def __init__(
        self, coordinator: ActronCoordinator, zone_index: int, zone
    ) -> None:
        """Initialise a zone climate entity."""
        super().__init__(coordinator)
        self._zone_index = zone_index
        zone_name = getattr(zone, "title", None) or f"Zone {zone_index}"
        self._attr_unique_id = f"{DOMAIN}_{coordinator.serial}_zone_{zone_index}"
        self._attr_name = f"Actron {zone_name}"

    @property
    def _status(self):
        return self.coordinator.data

    @property
    def _zone(self):
        return self._status.remote_zone_info[self._zone_index]

    @property
    def _parent_settings(self):
        return self._status.user_aircon_settings

    @property
    def hvac_modes(self) -> list[HVACMode]:
        """Zone supports OFF plus whatever the parent is doing."""
        parent_mode = self._parent_ha_mode()
        if parent_mode and parent_mode != HVACMode.OFF:
            return [HVACMode.OFF, parent_mode]
        return [HVACMode.OFF]

    def _parent_ha_mode(self) -> HVACMode:
        """Get the parent unit's current HA mode (overlay-aware)."""
        is_on = self.coordinator.get_optimistic("is_on", self._parent_settings.is_on)
        if not is_on:
            return HVACMode.OFF
        sdk_mode = self.coordinator.get_optimistic(
            "mode", self._parent_settings.mode
        )
        return SDK_TO_HA_MODE.get(sdk_mode, HVACMode.OFF)

    @property
    def hvac_mode(self) -> HVACMode:
        """Return current zone HVAC mode (overlay-aware)."""
        is_active = self.coordinator.get_optimistic(
            f"zone_{self._zone_index}_active",
            getattr(self._zone, "is_active", False),
        )
        if not is_active:
            return HVACMode.OFF
        return self._parent_ha_mode()

    @property
    def current_temperature(self) -> float | None:
        """Return current zone temperature."""
        return getattr(self._zone, "live_temp_c", None)

    @property
    def target_temperature(self) -> float | None:
        """Return zone target temperature based on parent mode."""
        parent_mode = self._parent_ha_mode()
        if parent_mode == HVACMode.HEAT:
            return self.coordinator.get_optimistic(
                f"zone_{self._zone_index}_temp_heat",
                getattr(self._zone, "temperature_setpoint_heat_c", None),
            )
        return self.coordinator.get_optimistic(
            f"zone_{self._zone_index}_temp_cool",
            getattr(self._zone, "temperature_setpoint_cool_c", None),
        )

    def _set_enabled_zone_local(self, enabled: bool) -> None:
        """Mutate the shared ``enabled_zones`` list for this zone.

        Concurrent zone commands build their SDK payloads from this list (the
        SDK sends the whole ``EnabledZones`` array), so it must reflect our
        intent between polls. The next coordinator poll replaces the object.
        """
        zones_list = self._status.user_aircon_settings.enabled_zones
        if self._zone_index < len(zones_list):
            zones_list[self._zone_index] = enabled

    def _other_confirmed_active_zones(self) -> int:
        """Count zones (other than this one) active *on the controller*.

        A zone with a pending overlay is an in-flight HA request, not confirmed
        controller state, so it is excluded — this is what lets us wait for a
        just-enabled zone to actually take effect before turning another off.
        """
        zones = getattr(self._status, "remote_zone_info", None) or []
        count = 0
        for i, z in enumerate(zones):
            if i == self._zone_index:
                continue
            if not getattr(z, "is_active", False):
                continue
            if self.coordinator.is_pending(f"zone_{i}_active"):
                continue
            count += 1
        return count

    async def _await_other_active_zone(self) -> bool:
        """Block until another zone is confirmed active on the controller.

        Returns True once another zone is active (safe to turn this one off),
        or False if the hold window elapses with no other active zone.
        """
        if self._other_confirmed_active_zones() > 0:
            return True
        _LOGGER.info(
            "Zone %s turn-off held: it is the last active zone, waiting up to "
            "%ss for another zone to activate on the controller",
            self._zone_index, LAST_ZONE_OFF_HOLD_SECONDS,
        )
        deadline = time.monotonic() + LAST_ZONE_OFF_HOLD_SECONDS
        while time.monotonic() < deadline:
            # Keep polling fast so a newly-enabled zone is detected promptly.
            self.coordinator.extend_burst()
            await asyncio.sleep(ZONE_CONFIRM_POLL_SECONDS)
            if self._other_confirmed_active_zones() > 0:
                return True
        return False

    async def _send_until_confirmed(self, key: str, enabled: bool) -> None:
        """Re-issue the zone enable command until the controller confirms it.

        The cloud sometimes accepts a command (HTTP 200) without applying it, so
        firing once is unreliable. We re-send every ``ZONE_RESEND_INTERVAL_SECONDS``
        until the overlay clears (confirmed/reconciled) or the timeout elapses.
        """
        deadline = time.monotonic() + ZONE_CONFIRM_TIMEOUT_SECONDS
        while True:
            # Re-assert our intent on the shared list before each send so
            # concurrent commands (and the SDK's full-array payload) stay correct
            # even if a poll has since reverted it to the server value.
            self._set_enabled_zone_local(enabled)
            try:
                async with self.coordinator.command_lock:
                    await self.coordinator.api.enable_zone(
                        self._status, self._zone_index, enabled
                    )
            except ActronRateLimitError:
                _LOGGER.warning(
                    "Zone %s toggle hit a rate limit; stopping re-sends and "
                    "leaving the overlay to reconcile",
                    self._zone_index,
                )
                return

            waited = 0.0
            while waited < ZONE_RESEND_INTERVAL_SECONDS:
                if not self.coordinator.is_pending(key):
                    return  # confirmed, or dropped by reconciliation
                if time.monotonic() >= deadline:
                    _LOGGER.warning(
                        "Zone %s toggle (enabled=%s) not confirmed within %ss",
                        self._zone_index, enabled, ZONE_CONFIRM_TIMEOUT_SECONDS,
                    )
                    return
                self.coordinator.extend_burst()
                await asyncio.sleep(ZONE_CONFIRM_POLL_SECONDS)
                waited += ZONE_CONFIRM_POLL_SECONDS

    async def _toggle_zone(self, enabled: bool) -> None:
        """Enable/disable this zone: optimistic overlay, last-zone guard, resend.

        Mirrors ``_run_command``'s overlay + per-slot cancellation, but adds:
          * a hold-until-another-zone-active guard before turning off the last
            active zone (which would shut down and latch off the whole system);
          * re-sending the command until the controller confirms it.
        The waits run *outside* ``command_lock`` so a concurrent zone turn-on
        can still reach the controller while we wait on it.
        """
        key = f"zone_{self._zone_index}_active"
        # Capture the true pre-command baseline BEFORE mutating enabled_zones
        # — z.is_active reads directly from enabled_zones.
        baseline = getattr(self._zone, "is_active", False)
        self._set_enabled_zone_local(enabled)

        self.coordinator.set_optimistic(
            {key: enabled},
            f"zone[{self._zone_index}].enable({enabled})",
            baselines={key: baseline},
        )

        pending = self._pending_commands.get("zone_active")
        if pending is not None and not pending.done():
            pending.cancel()
        current = asyncio.current_task()
        self._pending_commands["zone_active"] = current

        try:
            # Guard only when actually switching an active zone off and it is
            # currently the last one active — turning off an already-off zone is
            # a no-op and must not trip the hold.
            if (
                not enabled
                and baseline
                and self._other_confirmed_active_zones() == 0
            ):
                if not await self._await_other_active_zone():
                    _LOGGER.warning(
                        "Refusing to turn off zone %s: no other zone became "
                        "active within %ss (would shut down the system)",
                        self._zone_index, LAST_ZONE_OFF_HOLD_SECONDS,
                    )
                    # Revert optimistic state — the zone stays on.
                    self._set_enabled_zone_local(True)
                    self.coordinator.clear_optimistic([key])
                    raise HomeAssistantError(
                        "Cannot turn off the last active zone — this would shut "
                        "down the entire system. Turn on another zone first."
                    )
            await self._send_until_confirmed(key, enabled)
        except asyncio.CancelledError:
            if self._pending_commands.get("zone_active") is not current:
                _LOGGER.debug("Zone %s command superseded by newer request", self._zone_index)
                return
            raise
        except HomeAssistantError:
            raise
        except Exception:
            self.coordinator.clear_optimistic([key])
            raise
        finally:
            if self._pending_commands.get("zone_active") is current:
                self._pending_commands.pop("zone_active", None)

    async def async_turn_on(self) -> None:
        """Turn zone on."""
        await self._toggle_zone(True)

    async def async_turn_off(self) -> None:
        """Turn zone off."""
        await self._toggle_zone(False)

    async def async_set_hvac_mode(self, hvac_mode: HVACMode) -> None:
        """Turn zone on or off."""
        if hvac_mode == HVACMode.OFF:
            await self.async_turn_off()
        else:
            await self.async_turn_on()

    async def async_set_temperature(self, **kwargs) -> None:
        """Set zone target temperature."""
        temp = kwargs.get(ATTR_TEMPERATURE)
        if temp is None:
            return
        parent_mode = self._parent_ha_mode()
        key = (
            f"zone_{self._zone_index}_temp_heat"
            if parent_mode == HVACMode.HEAT
            else f"zone_{self._zone_index}_temp_cool"
        )
        await self._run_command(
            "zone_temperature",
            {key: temp},
            f"zone[{self._zone_index}].set_temperature({temp})",
            self.coordinator.api.set_zone_temperature(
                self._status, self._zone_index, temp
            ),
        )

    @property
    def extra_state_attributes(self) -> dict:
        """Expose a `pending` flag when this zone has unconfirmed commands."""
        i = self._zone_index
        return {
            "pending": self.coordinator.any_pending(
                [f"zone_{i}_active", f"zone_{i}_temp_cool", f"zone_{i}_temp_heat"]
            )
        }
