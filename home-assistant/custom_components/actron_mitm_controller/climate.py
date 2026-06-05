"""Climate entities: 1 master + 8 zones, all backed by the WS coordinator.

Spec rule: use the server-published `*_transitioning` value when present; otherwise the
authoritative value. No optimistic state, no retries — the firmware is expected to be
solid and API-level errors surface directly.
"""

import logging
from typing import Any

from homeassistant.components.climate import (
    ClimateEntity,
    ClimateEntityFeature,
    HVACMode,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import ATTR_TEMPERATURE, UnitOfTemperature
from homeassistant.core import HomeAssistant, callback
from homeassistant.exceptions import HomeAssistantError
from homeassistant.helpers.device_registry import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import (
    DOMAIN,
    FAN_FW_TO_HA,
    FAN_HA_TO_FW,
    MODES_FW_TO_HA,
    MODES_HA_TO_FW,
)
from .coordinator import ActronMitmCoordinator

_LOGGER = logging.getLogger(__name__)

NUM_ZONES = 8

# Match the firmware-side validation (state.cpp / ws_api.cpp).
MIN_TEMP = 10.0
MAX_TEMP = 35.0
TEMP_STEP = 0.5

HVAC_MODES_MASTER = [
    HVACMode.OFF,
    HVACMode.COOL,
    HVACMode.HEAT,
    HVACMode.HEAT_COOL,
    HVACMode.FAN_ONLY,
]
FAN_MODES = ["low", "medium", "high", "auto"]


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: ActronMitmCoordinator = hass.data[DOMAIN][entry.entry_id]
    entities: list[ClimateEntity] = [ActronMitmMaster(coordinator, entry)]
    for i in range(NUM_ZONES):
        entities.append(ActronMitmZone(coordinator, entry, i))
    async_add_entities(entities)


def _value_with_transition(data: dict, key: str) -> Any:
    """Spec rule: ha = ws_transitioning if not None else ws_value."""
    tr = data.get(f"{key}_transitioning")
    if tr is not None:
        return tr
    return data.get(key)


class _Base(CoordinatorEntity[ActronMitmCoordinator], ClimateEntity):
    """Shared availability + unit setup + device grouping."""

    _attr_temperature_unit = UnitOfTemperature.CELSIUS
    _attr_target_temperature_step = TEMP_STEP
    _attr_min_temp = MIN_TEMP
    _attr_max_temp = MAX_TEMP
    _attr_has_entity_name = True

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        super().__init__(coordinator)
        # Grouping all entities under a single Device so HA prefixes their entity_ids
        # with the device slug ("actron_ac") and surfaces them together in the UI.
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, entry.entry_id)},
            name="Actron AC",
            manufacturer="Actron",
            model="NEO via local MITM bridge",
            configuration_url=f"http://{coordinator.host}/",
        )

    @property
    def available(self) -> bool:  # type: ignore[override]
        return self.coordinator.is_available() and self.coordinator.data is not None


class ActronMitmMaster(_Base):
    """The master HVAC entity — mode, fan, master setpoint."""

    _attr_supported_features = (
        ClimateEntityFeature.TARGET_TEMPERATURE
        | ClimateEntityFeature.FAN_MODE
        | ClimateEntityFeature.TURN_ON
        | ClimateEntityFeature.TURN_OFF
    )
    _attr_hvac_modes = HVAC_MODES_MASTER
    _attr_fan_modes = FAN_MODES
    _attr_name = "Master"

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        super().__init__(coordinator, entry)
        self._attr_unique_id = f"{entry.entry_id}_master"

    @property
    def hvac_mode(self) -> HVACMode | None:
        data = self.coordinator.data or {}
        fw = _value_with_transition(data, "mode")
        return HVACMode(MODES_FW_TO_HA.get(fw)) if fw in MODES_FW_TO_HA else None

    @property
    def fan_mode(self) -> str | None:
        data = self.coordinator.data or {}
        fw = _value_with_transition(data, "fan")
        return FAN_FW_TO_HA.get(fw) if fw else None

    @property
    def current_temperature(self) -> float | None:
        # Indoor-unit main/return-air reading scraped from the bus (reg 13), published by the
        # bridge as the master's `current_temp`. Read-only — no transition counterpart.
        data = self.coordinator.data or {}
        return data.get("current_temp")

    @property
    def target_temperature(self) -> float | None:
        data = self.coordinator.data or {}
        return _value_with_transition(data, "master_setpoint")

    async def async_set_hvac_mode(self, hvac_mode: HVACMode) -> None:
        fw = MODES_HA_TO_FW.get(hvac_mode)
        if fw is None:
            raise HomeAssistantError(f"Unsupported HVAC mode: {hvac_mode}")
        try:
            await self.coordinator.async_send_command("set_mode", value=fw)
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err

    async def async_set_fan_mode(self, fan_mode: str) -> None:
        fw = FAN_HA_TO_FW.get(fan_mode)
        if fw is None:
            raise HomeAssistantError(f"Unsupported fan mode: {fan_mode}")
        try:
            await self.coordinator.async_send_command("set_fan", value=fw)
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err

    async def async_set_temperature(self, **kwargs: Any) -> None:
        temp = kwargs.get(ATTR_TEMPERATURE)
        if temp is None:
            return
        try:
            await self.coordinator.async_send_command(
                "set_master_setpoint", value=float(temp)
            )
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err


class ActronMitmZone(_Base):
    """One climate entity per RS485 zone slot (0..7)."""

    _attr_supported_features = (
        ClimateEntityFeature.TARGET_TEMPERATURE
        | ClimateEntityFeature.TURN_ON
        | ClimateEntityFeature.TURN_OFF
    )
    # Zone modes: OFF when disabled; otherwise it follows the master mode (cool/heat/auto).
    # FAN_ONLY is master-only — zones have no setpoint meaning in fan.
    _attr_hvac_modes = [HVACMode.OFF, HVACMode.COOL, HVACMode.HEAT, HVACMode.HEAT_COOL]

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry, zone_idx: int):
        super().__init__(coordinator, entry)
        self._zone = zone_idx
        self._attr_unique_id = f"{entry.entry_id}_zone_{zone_idx}"
        self._attr_name = f"Zone {zone_idx}"

    def _zone_data(self) -> dict[str, Any] | None:
        data = self.coordinator.data or {}
        zones = data.get("zones") or []
        for z in zones:
            if z.get("id") == self._zone:
                return z
        return None

    @property
    def hvac_mode(self) -> HVACMode | None:
        z = self._zone_data()
        if z is None:
            return None
        enabled = _value_with_transition(z, "enabled")
        if enabled is False:
            return HVACMode.OFF
        # Zone is enabled → mirror the master's mode (or its transition).
        master_mode_fw = _value_with_transition(self.coordinator.data or {}, "mode")
        return HVACMode(MODES_FW_TO_HA.get(master_mode_fw)) if master_mode_fw in MODES_FW_TO_HA else None

    @property
    def current_temperature(self) -> float | None:
        z = self._zone_data()
        return z.get("current_temp") if z else None

    @property
    def target_temperature(self) -> float | None:
        z = self._zone_data()
        if z is None:
            return None
        return _value_with_transition(z, "target_temp")

    async def async_set_hvac_mode(self, hvac_mode: HVACMode) -> None:
        # Only OFF and "enabled" (any non-off mode) are zone-level operations.
        # Changing mode to cool/heat/auto on a zone means "enable" — we don't override
        # the master's mode here.
        enabled = hvac_mode != HVACMode.OFF
        try:
            await self.coordinator.async_send_command(
                "set_zone_enabled", zone=self._zone, value=enabled
            )
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err

    async def async_set_temperature(self, **kwargs: Any) -> None:
        temp = kwargs.get(ATTR_TEMPERATURE)
        if temp is None:
            return
        try:
            await self.coordinator.async_send_command(
                "set_zone_setpoint", zone=self._zone, value=float(temp)
            )
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err

    async def async_turn_on(self) -> None:
        try:
            await self.coordinator.async_send_command(
                "set_zone_enabled", zone=self._zone, value=True
            )
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err

    async def async_turn_off(self) -> None:
        try:
            await self.coordinator.async_send_command(
                "set_zone_enabled", zone=self._zone, value=False
            )
        except RuntimeError as err:
            raise HomeAssistantError(str(err)) from err
