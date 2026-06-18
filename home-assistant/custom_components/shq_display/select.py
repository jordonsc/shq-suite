"""Select platform for SHQ Display integration."""
import logging
from typing import Optional

from homeassistant.components.select import SelectEntity
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity import EntityCategory
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN

_LOGGER = logging.getLogger(__name__)

IDLE_MODES = ["off", "clock"]


async def async_setup_platform(
    hass: HomeAssistant,
    config: dict,
    async_add_entities: AddEntitiesCallback,
    discovery_info: Optional[dict] = None,
):
    """Set up the SHQ Display select platform."""
    coordinators = hass.data.get(DOMAIN, {})
    entities = [SHQDisplayIdleMode(c) for c in coordinators.values()]
    async_add_entities(entities)


class SHQDisplayIdleMode(CoordinatorEntity, SelectEntity):
    """What the kiosk shows when idle: turn off, or display the clock overlay."""

    _attr_options = IDLE_MODES
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:clock-digital"

    def __init__(self, coordinator):
        """Initialize the idle-mode select."""
        super().__init__(coordinator)
        self._attr_name = f"{coordinator.name} Idle Mode"
        self._attr_unique_id = f"{DOMAIN}_{coordinator.device_id}_idle_mode"

    @property
    def current_option(self) -> Optional[str]:
        """Return the current idle mode."""
        if not self.coordinator.data:
            return None
        return self.coordinator.data.get('auto_dim', {}).get('idle_mode')

    async def async_select_option(self, option: str) -> None:
        """Set the idle mode, preserving the rest of the auto-dim config."""
        auto_dim_data = self.coordinator.data.get('auto_dim', {})
        await self.coordinator.async_send_command(
            self.coordinator.client.set_auto_dim_config,
            dim_level=auto_dim_data.get('dim_level', 25),
            bright_level=auto_dim_data.get('bright_level', 178),
            auto_dim_time=auto_dim_data.get('auto_dim_time', 0),
            auto_off_time=auto_dim_data.get('auto_off_time', 0),
            idle_mode=option,
        )
