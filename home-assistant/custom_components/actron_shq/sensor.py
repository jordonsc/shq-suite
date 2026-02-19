"""Sensor platform for Actron SHQ integration."""

import logging

from homeassistant.components.sensor import SensorDeviceClass, SensorEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import PERCENTAGE, UnitOfTemperature
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN
from .coordinator import ActronCoordinator

_LOGGER = logging.getLogger(__name__)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Set up Actron sensor entities from a config entry."""
    coordinator: ActronCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([
        ActronOutdoorTemperatureSensor(coordinator),
        ActronHumiditySensor(coordinator),
    ])


class ActronOutdoorTemperatureSensor(CoordinatorEntity, SensorEntity):
    """Outdoor temperature sensor."""

    _attr_device_class = SensorDeviceClass.TEMPERATURE
    _attr_native_unit_of_measurement = UnitOfTemperature.CELSIUS

    def __init__(self, coordinator: ActronCoordinator) -> None:
        """Initialise the sensor."""
        super().__init__(coordinator)
        self._attr_unique_id = f"{DOMAIN}_{coordinator.serial}_outdoor_temp"
        self._attr_name = "Actron Outdoor Temperature"

    @property
    def native_value(self) -> float | None:
        """Return the outdoor temperature."""
        if not self.coordinator.data:
            return None
        return self.coordinator.data.outdoor_temperature


class ActronHumiditySensor(CoordinatorEntity, SensorEntity):
    """Humidity sensor."""

    _attr_device_class = SensorDeviceClass.HUMIDITY
    _attr_native_unit_of_measurement = PERCENTAGE

    def __init__(self, coordinator: ActronCoordinator) -> None:
        """Initialise the sensor."""
        super().__init__(coordinator)
        self._attr_unique_id = f"{DOMAIN}_{coordinator.serial}_humidity"
        self._attr_name = "Actron Humidity"

    @property
    def native_value(self) -> float | None:
        """Return the humidity."""
        if not self.coordinator.data:
            return None
        return self.coordinator.data.humidity
