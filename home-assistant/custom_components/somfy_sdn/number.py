"""Number entity for Somfy SDN — per-motor jog nudge duration (CTRL_MOVE timed move).

Holds the duration the Jog up/down buttons use for the commissioning nudge (Set Pro style: each
press runs the motor for this long, then auto-stops). Local config value (no device round-trip);
stored on the coordinator so the buttons can read it, and restored across restarts. Units are the
raw SDN protocol duration (0x0A..0xFF = 10..255) — undocumented real-world time, dial in to taste.
"""

from homeassistant.components.number import NumberEntity, NumberMode, RestoreNumber
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnMotorEntity, add_motor_entities

DEFAULT_JOG_DURATION = 20


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    add_motor_entities(
        coordinator,
        entry,
        async_add_entities,
        lambda addr: [JogStepNumber(coordinator, addr), BottomLimitNumber(coordinator, addr)],
    )


class JogStepNumber(SomfySdnMotorEntity, RestoreNumber):
    _attr_name = "Jog duration"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:timer-cog"
    _attr_native_min_value = 10
    _attr_native_max_value = 255
    _attr_native_step = 1
    _attr_native_unit_of_measurement = "units"
    _attr_mode = NumberMode.BOX

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str) -> None:
        super().__init__(coordinator, addr)
        self._attr_unique_id = f"{self._cid}:{addr}:jog_duration"
        self._value = float(DEFAULT_JOG_DURATION)

    # Config entity — available regardless of motor online state.
    @property
    def available(self) -> bool:
        return True

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        last = await self.async_get_last_number_data()
        if last is not None and last.native_value is not None:
            self._value = float(last.native_value)
        self.coordinator.jog_steps[self._addr] = int(self._value)

    @property
    def native_value(self) -> float:
        return self._value

    async def async_set_native_value(self, value: float) -> None:
        self._value = value
        self.coordinator.jog_steps[self._addr] = int(value)
        self.async_write_ha_state()


class BottomLimitNumber(SomfySdnMotorEntity, NumberEntity):
    """Down (closed) limit as an absolute pulse count from the top (0) reference.

    Stateful: reads the motor's current down limit, and writing a value sets it on the motor
    (SetMotorLimits specified_position). The top limit is always the 0 reference (set at the
    current position), so only the bottom limit makes sense as an absolute pulse count.
    """

    _attr_name = "Bottom limit"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:arrow-collapse-down"
    _attr_native_min_value = 0
    _attr_native_max_value = 65535
    _attr_native_step = 1
    _attr_native_unit_of_measurement = "pulses"
    _attr_mode = NumberMode.BOX

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str) -> None:
        super().__init__(coordinator, addr)
        self._attr_unique_id = f"{self._cid}:{addr}:bottom_limit"

    @property
    def native_value(self) -> float | None:
        return self._device.get("down_limit")  # None until limits are known

    async def async_set_native_value(self, value: float) -> None:
        await self.coordinator.async_send_command(
            "set_bottom_limit_pulses", addr=self._addr, pulses=int(value)
        )
