"""Switch entities for Somfy SDN.

Controller: Bus active (ACTIVE/LISTEN TX gate). Per-motor: Reversed direction. Both are stateful
(read back from the firmware state) and config-category.
"""

from typing import Any

from homeassistant.components.switch import SwitchEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnControllerEntity, SomfySdnMotorEntity, add_motor_entities


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([BusActiveSwitch(coordinator)])
    add_motor_entities(
        coordinator, entry, async_add_entities, lambda addr: [ReversedSwitch(coordinator, addr)]
    )


class BusActiveSwitch(SomfySdnControllerEntity, SwitchEntity):
    _attr_name = "Bus active"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:transmission-tower"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:bus_active"

    @property
    def is_on(self) -> bool:
        return (self.coordinator.data or {}).get("mode") == "active"

    async def async_turn_on(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command("set_mode", mode="active")

    async def async_turn_off(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command("set_mode", mode="listen")


class ReversedSwitch(SomfySdnMotorEntity, SwitchEntity):
    _attr_name = "Reversed"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:swap-vertical"

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str) -> None:
        super().__init__(coordinator, addr)
        self._attr_unique_id = f"{self._cid}:{addr}:reversed"

    @property
    def is_on(self) -> bool:
        return self._device.get("direction") == "reversed"

    async def async_turn_on(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command("set_direction", addr=self._addr, reversed=True)

    async def async_turn_off(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command(
            "set_direction", addr=self._addr, reversed=False
        )
