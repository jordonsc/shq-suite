"""Diagnostic sensors for Somfy SDN (controller device): motors online + recent wire errors."""

from homeassistant.components.sensor import (
    SensorEntity,
    SensorStateClass,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnControllerEntity


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities(
        [MotorsOnlineSensor(coordinator), RecentErrorsSensor(coordinator)]
    )


class MotorsOnlineSensor(SomfySdnControllerEntity, SensorEntity):
    _attr_name = "Motors online"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_state_class = SensorStateClass.MEASUREMENT
    _attr_icon = "mdi:check-network"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:motors_online"

    @property
    def native_value(self) -> int:
        devs = (self.coordinator.data or {}).get("devices", [])
        return sum(1 for d in devs if d.get("online"))


class RecentErrorsSensor(SomfySdnControllerEntity, SensorEntity):
    _attr_name = "Wire errors"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_state_class = SensorStateClass.TOTAL_INCREASING
    _attr_icon = "mdi:alert-circle"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:errors"

    @property
    def native_value(self) -> int | None:
        return (self.coordinator.data or {}).get("errors_recent")
