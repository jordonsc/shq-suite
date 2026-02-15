"""Binary sensor platform for CFA Fire Ban integration."""
import logging
from typing import Any, Optional

from homeassistant.components.binary_sensor import BinarySensorEntity
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
    """Set up the CFA Total Fire Ban binary sensor."""
    data = hass.data[DOMAIN]
    async_add_entities([TotalFireBanSensor(data["coordinator"], data["name"], data["district"])])


class TotalFireBanSensor(CoordinatorEntity, BinarySensorEntity):
    """Binary sensor for Total Fire Ban status."""

    def __init__(self, coordinator, name: str, district: str) -> None:
        """Initialise the sensor."""
        super().__init__(coordinator)
        self._attr_name = f"{name} Total Fire Ban"
        self._attr_unique_id = f"{DOMAIN}_{district}_total_fire_ban"
        self._attr_icon = "mdi:fire-alert"

    @property
    def is_on(self) -> Optional[bool]:
        """Return true if a Total Fire Ban is active."""
        if not self.coordinator.data:
            return None
        return self.coordinator.data["tfb_active"]

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        """Return extra state attributes."""
        if not self.coordinator.data:
            return {}
        return {"date": self.coordinator.data["date"]}
