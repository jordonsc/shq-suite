"""Per-motor fault binary sensor for Somfy SDN."""

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnMotorEntity, add_motor_entities


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    add_motor_entities(
        coordinator, entry, async_add_entities, lambda addr: [FaultBinarySensor(coordinator, addr)]
    )


class FaultBinarySensor(SomfySdnMotorEntity, BinarySensorEntity):
    _attr_name = "Fault"
    _attr_device_class = BinarySensorDeviceClass.PROBLEM
    _attr_entity_category = EntityCategory.DIAGNOSTIC

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str) -> None:
        super().__init__(coordinator, addr)
        self._attr_unique_id = f"{self._cid}:{addr}:fault"

    @property
    def is_on(self) -> bool:
        return bool(self._device.get("fault"))
