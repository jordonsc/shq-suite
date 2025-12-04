"""Light platform for SHQ Display integration."""
import logging
from typing import Any, Optional

from homeassistant.components.light import (
    ATTR_BRIGHTNESS,
    ColorMode,
    LightEntity,
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
    """Set up the SHQ Display light platform."""
    entities = []

    # Get coordinators from hass.data
    coordinators = hass.data.get(DOMAIN, {})

    for device_id, coordinator in coordinators.items():
        entities.append(SHQDisplayLight(coordinator))

    async_add_entities(entities)


class SHQDisplayLight(CoordinatorEntity, LightEntity):
    """Representation of an SHQ Display as a light."""

    def __init__(self, coordinator):
        """Initialize the light."""
        super().__init__(coordinator)
        self._attr_name = coordinator.name
        self._attr_unique_id = f"{DOMAIN}_{coordinator.device_id}_light"
        self._attr_color_mode = ColorMode.BRIGHTNESS
        self._attr_supported_color_modes = {ColorMode.BRIGHTNESS}
        # Note: device_info not supported for YAML-based integrations

    @property
    def is_on(self) -> bool:
        """Return true if light is on."""
        if not self.coordinator.data:
            return True

        display_data = self.coordinator.data.get('display', {})
        brightness = display_data.get('brightness', 255)
        display_on = display_data.get('display_on', True)
        return display_on and brightness > 0

    @property
    def brightness(self) -> int:
        """Return the brightness of the light (0-255)."""
        if not self.coordinator.data:
            return 255

        display_data = self.coordinator.data.get('display', {})
        brightness = display_data.get('brightness', 255)  # 0-255 scale

        return brightness

    async def async_turn_on(self, **kwargs: Any) -> None:
        """Turn the light on."""
        brightness = kwargs.get(ATTR_BRIGHTNESS)

        if brightness is not None:
            # Brightness is already 0-255, no conversion needed
            await self.coordinator.async_send_command(
                self.coordinator.client.set_brightness, brightness
            )
        else:
            # Use wake command to turn on to bright level
            await self.coordinator.async_send_command(
                self.coordinator.client.wake
            )

    async def async_turn_off(self, **kwargs: Any) -> None:
        """Turn the light off using sleep command."""
        await self.coordinator.async_send_command(
            self.coordinator.client.sleep
        )
