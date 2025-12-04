"""Number platform for SHQ Display integration."""
import logging
from typing import Optional

from homeassistant.components.number import NumberEntity, NumberMode
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
    """Set up the SHQ Display number platform."""
    entities = []

    # Get coordinators from hass.data
    coordinators = hass.data.get(DOMAIN, {})

    for device_id, coordinator in coordinators.items():
        # Create number entities for each timer
        entities.append(SHQDisplayDimLevel(coordinator))
        entities.append(SHQDisplayBrightLevel(coordinator))
        entities.append(SHQDisplayDimTime(coordinator))
        entities.append(SHQDisplayOffTime(coordinator))

    async_add_entities(entities)


class SHQDisplayNumberBase(CoordinatorEntity, NumberEntity):
    """Base class for SHQ Display number entities."""

    def __init__(self, coordinator, entity_type: str, config_key: str):
        """Initialize the number entity."""
        super().__init__(coordinator)
        self._entity_type = entity_type
        self._config_key = config_key
        self._attr_name = f"{coordinator.name} {entity_type}"
        self._attr_unique_id = f"{DOMAIN}_{coordinator.device_id}_{entity_type.lower().replace(' ', '_')}"
        self._attr_mode = NumberMode.BOX
        # Note: device_info not supported for YAML-based integrations

    @property
    def native_value(self) -> Optional[float]:
        """Return the current value."""
        if not self.coordinator.data:
            return None

        auto_dim_data = self.coordinator.data.get('auto_dim', {})
        return auto_dim_data.get(self._config_key)


class SHQDisplayDimLevel(SHQDisplayNumberBase):
    """Dim brightness level (0-255)."""

    def __init__(self, coordinator):
        """Initialize the dim level entity."""
        super().__init__(coordinator, "Dim Level", "dim_level")
        self._attr_native_min_value = 0
        self._attr_native_max_value = 255
        self._attr_native_step = 1
        self._attr_native_unit_of_measurement = None

    async def async_set_native_value(self, value: float) -> None:
        """Set the dim level."""
        # Get current config to preserve other values
        auto_dim_data = self.coordinator.data.get('auto_dim', {})
        await self.coordinator.async_send_command(
            self.coordinator.client.set_auto_dim_config,
            dim_level=int(value),
            bright_level=auto_dim_data.get('bright_level', 178),
            auto_dim_time=auto_dim_data.get('auto_dim_time', 0),
            auto_off_time=auto_dim_data.get('auto_off_time', 0)
        )


class SHQDisplayBrightLevel(SHQDisplayNumberBase):
    """Bright brightness level (1-255)."""

    def __init__(self, coordinator):
        """Initialize the bright level entity."""
        super().__init__(coordinator, "Bright Level", "bright_level")
        self._attr_native_min_value = 1
        self._attr_native_max_value = 255
        self._attr_native_step = 1
        self._attr_native_unit_of_measurement = None

    async def async_set_native_value(self, value: float) -> None:
        """Set the bright level."""
        # Get current config to preserve other values
        auto_dim_data = self.coordinator.data.get('auto_dim', {})
        await self.coordinator.async_send_command(
            self.coordinator.client.set_auto_dim_config,
            dim_level=auto_dim_data.get('dim_level', 25),
            bright_level=int(value),
            auto_dim_time=auto_dim_data.get('auto_dim_time', 0),
            auto_off_time=auto_dim_data.get('auto_off_time', 0)
        )


class SHQDisplayDimTime(SHQDisplayNumberBase):
    """Auto-dim time in seconds."""

    def __init__(self, coordinator):
        """Initialize the dim time entity."""
        super().__init__(coordinator, "Dim Time", "auto_dim_time")
        self._attr_native_min_value = 0
        self._attr_native_max_value = 3600
        self._attr_native_step = 1
        self._attr_native_unit_of_measurement = "s"

    async def async_set_native_value(self, value: float) -> None:
        """Set the auto-dim time."""
        # Get current config to preserve other values
        auto_dim_data = self.coordinator.data.get('auto_dim', {})
        await self.coordinator.async_send_command(
            self.coordinator.client.set_auto_dim_config,
            dim_level=auto_dim_data.get('dim_level', 25),
            bright_level=auto_dim_data.get('bright_level', 178),
            auto_dim_time=int(value),
            auto_off_time=auto_dim_data.get('auto_off_time', 0)
        )


class SHQDisplayOffTime(SHQDisplayNumberBase):
    """Auto-off time in seconds."""

    def __init__(self, coordinator):
        """Initialize the off time entity."""
        super().__init__(coordinator, "Off Time", "auto_off_time")
        self._attr_native_min_value = 0
        self._attr_native_max_value = 3600
        self._attr_native_step = 1
        self._attr_native_unit_of_measurement = "s"

    async def async_set_native_value(self, value: float) -> None:
        """Set the auto-off time."""
        # Get current config to preserve other values
        auto_dim_data = self.coordinator.data.get('auto_dim', {})
        await self.coordinator.async_send_command(
            self.coordinator.client.set_auto_dim_config,
            dim_level=auto_dim_data.get('dim_level', 25),
            bright_level=auto_dim_data.get('bright_level', 178),
            auto_dim_time=auto_dim_data.get('auto_dim_time', 0),
            auto_off_time=int(value)
        )
