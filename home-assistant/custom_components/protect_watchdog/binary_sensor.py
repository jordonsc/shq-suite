"""Binary sensor flagging that the Protect event websocket has stopped delivering."""

from __future__ import annotations

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.typing import ConfigType, DiscoveryInfoType
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN
from .coordinator import ProtectWatchdogCoordinator


async def async_setup_platform(
    hass: HomeAssistant,
    config: ConfigType,
    async_add_entities: AddEntitiesCallback,
    discovery_info: DiscoveryInfoType | None = None,
) -> None:
    """Set up the watchdog binary sensor."""
    if discovery_info is None:
        return
    async_add_entities([ProtectEventStreamStale(hass.data[DOMAIN])])


class ProtectEventStreamStale(
    CoordinatorEntity[ProtectWatchdogCoordinator], BinarySensorEntity
):
    """On when Protect detections can no longer reach HA.

    This is the entity to gate camera automations on, and the one to alert from:
    when it is on, every Protect detection binary_sensor is frozen at its last
    value and will never change again on its own.
    """

    _attr_has_entity_name = False
    _attr_name = "Protect Event Stream Stale"
    _attr_unique_id = f"{DOMAIN}_stale"
    _attr_device_class = BinarySensorDeviceClass.PROBLEM
    _attr_icon = "mdi:lan-disconnect"

    @property
    def is_on(self) -> bool:
        """Whether the event stream has been silent past the threshold."""
        return self.coordinator.is_stale

    @property
    def extra_state_attributes(self) -> dict[str, object]:
        """Enough context to act on the alert without re-deriving it."""
        data = self.coordinator.data
        return {
            "seconds_since_healthy": round(self.coordinator.seconds_since_healthy, 1),
            "stale_after": self.coordinator.stale_after,
            "sockets": None if data is None else data.sockets,
            "nvr_host": self.coordinator.nvr_host,
            "remedy": (
                "Reload the unifiprotect config entry: POST "
                "/api/config/config_entries/entry/<entry_id>/reload"
            ),
        }
