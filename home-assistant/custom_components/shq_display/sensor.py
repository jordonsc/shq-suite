"""Sensor platform for SHQ Display integration."""
import logging
from typing import Optional

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
):
    """Set up the SHQ Display sensor platform."""
    entities = []

    # Get coordinators from hass.data
    coordinators = hass.data.get(DOMAIN, {})

    for device_id, coordinator in coordinators.items():
        entities.append(SHQDisplayVersionSensor(coordinator))
        entities.append(SHQDisplayUrlSensor(coordinator))

    async_add_entities(entities)


class SHQDisplayVersionSensor(CoordinatorEntity, SensorEntity):
    """Version sensor for SHQ Display."""

    def __init__(self, coordinator):
        """Initialize the version sensor."""
        super().__init__(coordinator)
        self._attr_name = f"{coordinator.name} Version"
        self._attr_unique_id = f"{DOMAIN}_{coordinator.device_id}_version"
        self._attr_icon = "mdi:information-outline"
        # Note: device_info not supported for YAML-based integrations

    @property
    def native_value(self) -> Optional[str]:
        """Return the version."""
        if not self.coordinator.data:
            return None

        return self.coordinator.data.get('version', 'Unknown')

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()


class SHQDisplayUrlSensor(CoordinatorEntity, SensorEntity):
    """URL sensor for SHQ Display."""

    def __init__(self, coordinator):
        """Initialize the URL sensor."""
        super().__init__(coordinator)
        self._attr_name = f"{coordinator.name} URL"
        self._attr_unique_id = f"{DOMAIN}_{coordinator.device_id}_url"
        self._attr_icon = "mdi:web"

    @property
    def native_value(self) -> Optional[str]:
        """Return the current URL."""
        if not self.coordinator.data:
            return None
        return self.coordinator.data.get('url')

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()
