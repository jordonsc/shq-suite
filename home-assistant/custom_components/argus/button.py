"""Button platform for the Argus integration."""
import logging
from typing import Optional

from homeassistant.components.button import ButtonEntity
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
    """Set up the Argus button platform."""
    coordinator = hass.data.get(DOMAIN)
    if coordinator is None:
        return

    async_add_entities([
        ArgusAcknowledgeButton(coordinator),
        ArgusStanddownButton(coordinator),
    ])


class ArgusAcknowledgeButton(CoordinatorEntity, ButtonEntity):
    """Acknowledge the active Argus case."""

    _attr_icon = "mdi:check-circle-outline"

    def __init__(self, coordinator):
        """Initialize the acknowledge button."""
        super().__init__(coordinator)
        self._attr_name = f"{coordinator.friendly_name} Acknowledge"
        self._attr_unique_id = f"{DOMAIN}_acknowledge"

    async def async_press(self) -> None:
        """Send the acknowledge command."""
        await self.coordinator.async_send_command(
            self.coordinator.client.acknowledge
        )

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()


class ArgusStanddownButton(CoordinatorEntity, ButtonEntity):
    """Stand down (clear) the active Argus case."""

    _attr_icon = "mdi:shield-off-outline"

    def __init__(self, coordinator):
        """Initialize the standdown button."""
        super().__init__(coordinator)
        self._attr_name = f"{coordinator.friendly_name} Standdown"
        self._attr_unique_id = f"{DOMAIN}_standdown"

    async def async_press(self) -> None:
        """Send the standdown command."""
        await self.coordinator.async_send_command(
            self.coordinator.client.standdown
        )

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()
