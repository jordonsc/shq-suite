"""Binary sensor platform for the Argus integration."""
import logging
from typing import Any, Dict, Optional

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
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
):
    """Set up the Argus binary sensor platform."""
    coordinator = hass.data.get(DOMAIN)
    if coordinator is None:
        return

    async_add_entities([ArgusActiveBinarySensor(coordinator)])


class ArgusActiveBinarySensor(CoordinatorEntity, BinarySensorEntity):
    """On when Argus has an alarm case in progress."""

    _attr_device_class = BinarySensorDeviceClass.SAFETY
    _attr_icon = "mdi:cctv"

    def __init__(self, coordinator):
        """Initialize the active binary sensor."""
        super().__init__(coordinator)
        self._attr_name = f"{coordinator.friendly_name} Active"
        self._attr_unique_id = f"{DOMAIN}_active"

    @property
    def is_on(self) -> Optional[bool]:
        """Return True when a case is in progress (safety = problem detected)."""
        if not self.coordinator.data:
            return None
        return bool(self.coordinator.data.get("active"))

    @property
    def extra_state_attributes(self) -> Dict[str, Any]:
        """Expose case context for automations/templates."""
        data = self.coordinator.data or {}
        return {
            "case_id": data.get("case_id"),
            "case_status": data.get("case_status"),
            "threat_level": data.get("threat_level"),
            "intruder_count": data.get("intruder_count"),
            "updated_at": data.get("updated_at"),
        }

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()
