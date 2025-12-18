"""Cover platform for DOSA door."""
import logging
from typing import Any, Optional

from homeassistant.components.cover import (
    CoverEntity,
    CoverEntityFeature,
    CoverDeviceClass,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.entity import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN
from .coordinator import DosaCoordinator

_LOGGER = logging.getLogger(__name__)


async def async_setup_platform(hass, config, async_add_entities, discovery_info=None):
    """Set up the DOSA cover platform."""
    if discovery_info is None:
        return

    coordinators = hass.data[DOMAIN]
    entities = []

    for device_id, coordinator in coordinators.items():
        entities.append(DosaCover(coordinator, device_id))

    async_add_entities(entities, True)


class DosaCover(CoordinatorEntity, CoverEntity):
    """Representation of a DOSA door as a cover."""

    _attr_device_class = CoverDeviceClass.DOOR
    _attr_supported_features = (
        CoverEntityFeature.OPEN
        | CoverEntityFeature.CLOSE
        | CoverEntityFeature.STOP
        | CoverEntityFeature.SET_POSITION
    )

    def __init__(self, coordinator: DosaCoordinator, device_id: str):
        """Initialize the cover."""
        super().__init__(coordinator)
        self._device_id = device_id
        self._attr_unique_id = f"{device_id}_door"
        self._attr_name = f"{coordinator.name} Door"

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
    def is_closed(self) -> Optional[bool]:
        """Return if the cover is closed."""
        if not self.coordinator.data:
            return None

        door = self.coordinator.data.get("door", {})
        state = door.get("state")

        # Return True only if closed, False for all other states except fault/pending
        if state == "closed":
            return True
        elif state in ("open", "intermediate", "opening", "closing", "halting", "homing"):
            return False
        # Only return None for truly unknown states (fault, pending, alarm, or missing)
        return None

    @property
    def is_opening(self) -> bool:
        """Return if the cover is opening."""
        if not self.coordinator.data:
            return False

        door = self.coordinator.data.get("door", {})
        state = door.get("state")
        # Treat homing as opening since it's a similar motion
        return state in ("opening", "homing")

    @property
    def is_closing(self) -> bool:
        """Return if the cover is closing."""
        if not self.coordinator.data:
            return False

        door = self.coordinator.data.get("door", {})
        state = door.get("state")
        # Treat halting as closing since it's decelerating/stopping
        return state in ("closing", "halting")

    @property
    def current_cover_position(self) -> Optional[int]:
        """Return current position of cover (0 closed, 100 open)."""
        if not self.coordinator.data:
            return None

        door = self.coordinator.data.get("door", {})
        position_percent = door.get("position_percent")

        if position_percent is not None:
            # Convert to integer (0-100)
            return int(round(position_percent))
        return None

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        """Return additional state attributes."""
        if not self.coordinator.data:
            return {}

        door = self.coordinator.data.get("door", {})
        attrs = {
            "state": door.get("state", "unknown"),
            "position_mm": door.get("position_mm", 0),
        }

        # Add fault message if present
        if fault_msg := door.get("fault_message"):
            attrs["fault_message"] = fault_msg

        # Add alarm code if present
        if alarm_code := door.get("alarm_code"):
            attrs["alarm_code"] = alarm_code

        return attrs

    @property
    def available(self) -> bool:
        """Return if entity is available."""
        if not self.coordinator.data:
            return False

        door = self.coordinator.data.get("door", {})
        state = door.get("state")

        # Entity is unavailable if in fault state
        return state != "fault"

    async def async_open_cover(self, **kwargs: Any) -> None:
        """Open the cover."""
        await self.coordinator.async_send_command(
            self.coordinator.client.open_door
        )

    async def async_close_cover(self, **kwargs: Any) -> None:
        """Close the cover."""
        await self.coordinator.async_send_command(
            self.coordinator.client.close_door
        )

    async def async_stop_cover(self, **kwargs: Any) -> None:
        """Stop the cover."""
        await self.coordinator.async_send_command(
            self.coordinator.client.stop
        )

    async def async_set_cover_position(self, **kwargs: Any) -> None:
        """Move the cover to a specific position."""
        position = kwargs.get("position")
        if position is not None:
            await self.coordinator.async_send_command(
                self.coordinator.client.move_to_percent,
                float(position)
            )
