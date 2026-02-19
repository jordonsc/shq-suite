"""Climate platform for Actron SHQ integration."""

import asyncio
import logging

from homeassistant.components.climate import (
    ClimateEntity,
    ClimateEntityFeature,
    HVACMode,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import ATTR_TEMPERATURE, UnitOfTemperature
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN
from .coordinator import ActronCoordinator

_LOGGER = logging.getLogger(__name__)

# HA mode -> SDK mode
HA_TO_SDK_MODE = {
    HVACMode.OFF: "OFF",
    HVACMode.COOL: "COOL",
    HVACMode.HEAT: "HEAT",
    HVACMode.AUTO: "AUTO",
    HVACMode.FAN_ONLY: "FAN",
}

# SDK mode -> HA mode
SDK_TO_HA_MODE = {v: k for k, v in HA_TO_SDK_MODE.items()}

# SDK fan mode -> HA fan mode (lowercase for HA)
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
    """Base class for Actron climate entities with command cancellation.

    Tracks in-flight API commands per slot (e.g. "temperature", "mode").
    When a new command arrives for the same slot, the previous one is
    cancelled — its retry sleeps and API waits get interrupted via
    CancelledError, and only the latest command's refresh runs.
    """

    _attr_temperature_unit = UnitOfTemperature.CELSIUS

    def __init__(self, coordinator: ActronCoordinator) -> None:
        """Initialise base climate entity."""
        super().__init__(coordinator)
        self._pending_commands: dict[str, asyncio.Task] = {}

    async def _execute_command(self, key: str, coro) -> None:
        """Run a command, cancelling any in-flight command for the same slot."""
        pending = self._pending_commands.get(key)
        if pending is not None and not pending.done():
            pending.cancel()

        current = asyncio.current_task()
        self._pending_commands[key] = current

        try:
            await coro
            await self.coordinator.async_request_refresh()
        except asyncio.CancelledError:
            # Check if we were superseded by a newer command (intentional)
            # vs cancelled by HA shutdown (propagate)
            if self._pending_commands.get(key) is not current:
                _LOGGER.debug("Command '%s' superseded by newer request", key)
                return
            raise
        finally:
            if self._pending_commands.get(key) is current:
                self._pending_commands.pop(key, None)


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
        """Return current HVAC mode."""
        if not self._settings.is_on:
            return HVACMode.OFF
        sdk_mode = self._settings.mode
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
        if mode in (HVACMode.COOL, HVACMode.AUTO, HVACMode.FAN_ONLY):
            return self._settings.temperature_setpoint_cool_c
        if mode == HVACMode.HEAT:
            return self._settings.temperature_setpoint_heat_c
        return self._settings.temperature_setpoint_cool_c

    @property
    def fan_mode(self) -> str | None:
        """Return current fan mode."""
        sdk_fan = self._settings.fan_mode
        return SDK_TO_HA_FAN.get(sdk_fan, None)

    async def async_set_hvac_mode(self, hvac_mode: HVACMode) -> None:
        """Set HVAC mode."""
        sdk_mode = HA_TO_SDK_MODE.get(hvac_mode)
        if sdk_mode is None:
            return
        await self._execute_command(
            "mode",
            self.coordinator.api.set_mode(self._status, sdk_mode),
        )

    async def async_set_temperature(self, **kwargs) -> None:
        """Set target temperature."""
        temp = kwargs.get(ATTR_TEMPERATURE)
        if temp is None:
            return
        await self._execute_command(
            "temperature",
            self.coordinator.api.set_temperature(self._status, temp),
        )

    async def async_set_fan_mode(self, fan_mode: str) -> None:
        """Set fan mode."""
        sdk_fan = HA_TO_SDK_FAN.get(fan_mode)
        if sdk_fan is None:
            return
        await self._execute_command(
            "fan_mode",
            self.coordinator.api.set_fan_mode(self._status, sdk_fan),
        )


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
        self._optimistic_active: bool | None = None
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
        """Get the parent unit's current HA mode."""
        if not self._parent_settings.is_on:
            return HVACMode.OFF
        sdk_mode = self._parent_settings.mode
        return SDK_TO_HA_MODE.get(sdk_mode, HVACMode.OFF)

    @property
    def hvac_mode(self) -> HVACMode:
        """Return current zone HVAC mode."""
        if self._optimistic_active is not None:
            is_active = self._optimistic_active
        else:
            is_active = getattr(self._zone, "is_active", False)
        if not is_active:
            return HVACMode.OFF
        return self._parent_ha_mode()

    def _handle_coordinator_update(self) -> None:
        """Clear optimistic state when real data arrives."""
        self._optimistic_active = None
        super()._handle_coordinator_update()

    @property
    def current_temperature(self) -> float | None:
        """Return current zone temperature."""
        return getattr(self._zone, "live_temp_c", None)

    @property
    def target_temperature(self) -> float | None:
        """Return zone target temperature based on parent mode."""
        parent_mode = self._parent_ha_mode()
        if parent_mode in (HVACMode.COOL, HVACMode.AUTO, HVACMode.FAN_ONLY):
            return getattr(self._zone, "temperature_setpoint_cool_c", None)
        if parent_mode == HVACMode.HEAT:
            return getattr(self._zone, "temperature_setpoint_heat_c", None)
        return getattr(self._zone, "temperature_setpoint_cool_c", None)

    async def _optimistic_zone_toggle(self, enabled: bool) -> None:
        """Toggle zone with optimistic state update.

        Sets the state immediately for responsive UI, then sends the API
        command. On failure, reverts the optimistic state.
        """
        self._optimistic_active = enabled
        self.coordinator.reset_poll_timer()
        self.async_write_ha_state()

        try:
            await self._execute_command(
                "mode",
                self.coordinator.api.enable_zone(
                    self._status, self._zone_index, enabled
                ),
            )
        except Exception:
            self._optimistic_active = None
            self.async_write_ha_state()
            raise

    async def async_turn_on(self) -> None:
        """Turn zone on (enable, inheriting parent mode)."""
        await self._optimistic_zone_toggle(True)

    async def async_turn_off(self) -> None:
        """Turn zone off (disable)."""
        await self._optimistic_zone_toggle(False)

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
        await self._execute_command(
            "temperature",
            self.coordinator.api.set_zone_temperature(
                self._status, self._zone_index, temp
            ),
        )
