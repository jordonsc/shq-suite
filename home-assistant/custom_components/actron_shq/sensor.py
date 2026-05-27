"""Sensor platform for Actron SHQ integration."""

import logging

from homeassistant.components.sensor import SensorDeviceClass, SensorEntity
from homeassistant.helpers.entity import EntityCategory
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
        ActronControllerStateSensor(coordinator),
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


class ActronControllerStateSensor(CoordinatorEntity, SensorEntity):
    """Diagnostic sensor exposing coordinator state.

    States: ``idle``, ``pending``, ``timeout``, ``rate_limited``. Attributes
    include the pending overlay keys, last command, burst window, and poll
    interval — useful for debugging why a commanded change hasn't shown up.
    """

    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_icon = "mdi:heart-pulse"
    _attr_options = ["idle", "pending", "timeout", "rate_limited"]
    _attr_device_class = SensorDeviceClass.ENUM

    def __init__(self, coordinator: ActronCoordinator) -> None:
        """Initialise the sensor."""
        super().__init__(coordinator)
        self._attr_unique_id = f"{DOMAIN}_{coordinator.serial}_controller_state"
        self._attr_name = "Actron Controller State"

    @property
    def available(self) -> bool:
        """Always available so rate-limited state stays visible."""
        return True

    @property
    def native_value(self) -> str:
        """Return the current controller state."""
        return self.coordinator.controller_state

    @property
    def extra_state_attributes(self) -> dict:
        """Return diagnostic attributes."""
        return self.coordinator.controller_state_attributes
