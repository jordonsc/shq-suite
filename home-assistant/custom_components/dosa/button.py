"""Button platform for DOSA door."""
import logging

from homeassistant.components.button import ButtonEntity, ButtonDeviceClass
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN
from .coordinator import DosaCoordinator

_LOGGER = logging.getLogger(__name__)


async def async_setup_platform(hass, config, async_add_entities, discovery_info=None):
    """Set up the DOSA button platform."""
    if discovery_info is None:
        return

    coordinators = hass.data[DOMAIN]
    entities = []

    for device_id, coordinator in coordinators.items():
        entities.extend([
            DosaHomeButton(coordinator, device_id),
            DosaZeroButton(coordinator, device_id),
            DosaClearAlarmButton(coordinator, device_id),
        ])

    async_add_entities(entities, True)


class DosaButtonBase(CoordinatorEntity, ButtonEntity):
    """Base class for DOSA buttons."""

    def __init__(self, coordinator: DosaCoordinator, device_id: str, button_type: str, name: str):
        """Initialize the button."""
        super().__init__(coordinator)
        self._device_id = device_id
        self._button_type = button_type
        self._attr_unique_id = f"{device_id}_{button_type}"
        self._attr_name = f"{coordinator.name} {name}"

    @property
    def device_info(self) -> DeviceInfo:
        """Return device information."""
        return DeviceInfo(
            identifiers={(DOMAIN, self._device_id)},
            name=self.coordinator.name,
            manufacturer="DOSA",
            model="Door Controller",
        )

    @property
    def available(self) -> bool:
        """Return if entity is available."""
        if not self.coordinator.data:
            return False

        door = self.coordinator.data.get("door", {})
        state = door.get("state")

        # Entity is unavailable if in fault state
        return state != "fault"


class DosaHomeButton(DosaButtonBase):
    """Button to home the DOSA door."""

    _attr_icon = "mdi:home-import-outline"

    def __init__(self, coordinator: DosaCoordinator, device_id: str):
        """Initialize the home button."""
        super().__init__(coordinator, device_id, "home", "Home")

    async def async_press(self) -> None:
        """Handle the button press."""
        await self.coordinator.async_send_command(
            self.coordinator.client.home
        )


class DosaZeroButton(DosaButtonBase):
    """Button to zero the DOSA door at current position."""

    _attr_icon = "mdi:target"

    def __init__(self, coordinator: DosaCoordinator, device_id: str):
        """Initialize the zero button."""
        super().__init__(coordinator, device_id, "zero", "Zero")

    async def async_press(self) -> None:
        """Handle the button press."""
        await self.coordinator.async_send_command(
            self.coordinator.client.zero
        )


class DosaClearAlarmButton(DosaButtonBase):
    """Button to clear alarm on the DOSA controller."""

    _attr_icon = "mdi:alarm-off"
    _attr_device_class = ButtonDeviceClass.RESTART

    def __init__(self, coordinator: DosaCoordinator, device_id: str):
        """Initialize the clear alarm button."""
        super().__init__(coordinator, device_id, "clear_alarm", "Clear Alarm")

    async def async_press(self) -> None:
        """Handle the button press."""
        await self.coordinator.async_send_command(
            self.coordinator.client.clear_alarm
        )
