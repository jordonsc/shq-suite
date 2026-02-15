"""Sensor platform for CFA Fire Ban integration."""
import logging
from typing import Any, Optional

from homeassistant.components.sensor import SensorEntity
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN

_LOGGER = logging.getLogger(__name__)


async def async_setup_platform(
    hass: HomeAssistant,
    config: dict,
    async_add_entities: AddEntitiesCallback,
    discovery_info: Optional[dict] = None,
) -> None:
    """Set up the CFA Fire Danger Rating sensor."""
    data = hass.data[DOMAIN]
    async_add_entities([FireDangerRatingSensor(data["coordinator"], data["name"], data["district"])])


class FireDangerRatingSensor(CoordinatorEntity, SensorEntity):
    """Sensor for fire danger rating level."""

    def __init__(self, coordinator, name: str, district: str) -> None:
        """Initialise the sensor."""
        super().__init__(coordinator)
        self._attr_name = f"{name} Fire Danger Rating"
        self._attr_unique_id = f"{DOMAIN}_{district}_fire_danger_rating"
        self._attr_icon = "mdi:fire"

    @property
    def native_value(self) -> Optional[str]:
        """Return the current fire danger rating."""
        if not self.coordinator.data:
            return None
        return self.coordinator.data["fire_danger_rating"]

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        """Return extra state attributes."""
        if not self.coordinator.data:
            return {}
        return {"date": self.coordinator.data["date"]}
