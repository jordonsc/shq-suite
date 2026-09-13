"""Sensor reporting how long the Protect event websocket has been silent."""

from __future__ import annotations

from homeassistant.components.sensor import (
    SensorDeviceClass,
    SensorEntity,
    SensorStateClass,
)
from homeassistant.const import UnitOfTime
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
    """Set up the watchdog sensor."""
    if discovery_info is None:
        return
    async_add_entities([ProtectWebsocketAgeSensor(hass.data[DOMAIN])])


class ProtectWebsocketAgeSensor(
    CoordinatorEntity[ProtectWatchdogCoordinator], SensorEntity
):
    """Seconds since the Protect event websocket last received a byte."""

    _attr_has_entity_name = False
    _attr_name = "Protect Event Stream Last RX"
    _attr_unique_id = f"{DOMAIN}_last_rx"
    _attr_device_class = SensorDeviceClass.DURATION
    _attr_native_unit_of_measurement = UnitOfTime.SECONDS
    _attr_state_class = SensorStateClass.MEASUREMENT
    _attr_icon = "mdi:lan-connect"
    _attr_entity_registry_enabled_default = True

    @property
    def native_value(self) -> float | None:
        """Age of the last received byte, or None when there is no socket."""
        data = self.coordinator.data
        return None if data is None else data.age

    @property
    def extra_state_attributes(self) -> dict[str, object]:
        """Expose the raw measurement, so a stale reading can be explained."""
        data = self.coordinator.data
        return {
            "sockets": None if data is None else data.sockets,
            "local_port": None if data is None else data.port,
            "bytes_received": None if data is None else data.bytes_received,
            "seconds_since_healthy": round(self.coordinator.seconds_since_healthy, 1),
            "stale_after": self.coordinator.stale_after,
        }
