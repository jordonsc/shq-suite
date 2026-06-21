"""Sensor platform for the Argus integration."""
import logging
from typing import Optional

from homeassistant.components.sensor import SensorEntity, SensorStateClass
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
    """Set up the Argus sensor platform."""
    coordinator = hass.data.get(DOMAIN)
    if coordinator is None:
        return

    async_add_entities([
        ArgusThreatLevelSensor(coordinator),
        ArgusIntruderCountSensor(coordinator),
        ArgusSummarySensor(coordinator),
        ArgusCaseIdSensor(coordinator),
        ArgusStatusSensor(coordinator),
    ])


class ArgusSensorBase(CoordinatorEntity, SensorEntity):
    """Base class for Argus sensors keyed off a single status field."""

    _data_key: str = ""

    def __init__(self, coordinator, suffix: str, unique_suffix: str):
        """Initialize the sensor."""
        super().__init__(coordinator)
        self._attr_name = f"{coordinator.friendly_name} {suffix}"
        self._attr_unique_id = f"{DOMAIN}_{unique_suffix}"

    @property
    def native_value(self):
        """Return the value of this sensor's status field."""
        if not self.coordinator.data:
            return None
        return self.coordinator.data.get(self._data_key)

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()


class ArgusThreatLevelSensor(ArgusSensorBase):
    """Current threat level (info | elevated | critical)."""

    _data_key = "threat_level"
    _attr_icon = "mdi:shield-alert"

    def __init__(self, coordinator):
        super().__init__(coordinator, "Threat Level", "threat_level")


class ArgusIntruderCountSensor(ArgusSensorBase):
    """Number of intruders currently assessed in the case."""

    _data_key = "intruder_count"
    _attr_icon = "mdi:account-alert"
    _attr_state_class = SensorStateClass.MEASUREMENT
    _attr_native_unit_of_measurement = "intruders"

    def __init__(self, coordinator):
        super().__init__(coordinator, "Intruder Count", "intruder_count")


class ArgusSummarySensor(ArgusSensorBase):
    """The current who/what/where assessment summary."""

    _data_key = "summary"
    _attr_icon = "mdi:text-box-outline"

    def __init__(self, coordinator):
        super().__init__(coordinator, "Summary", "summary")


class ArgusCaseIdSensor(ArgusSensorBase):
    """Identifier of the active case (null when idle)."""

    _data_key = "case_id"
    _attr_icon = "mdi:identifier"

    def __init__(self, coordinator):
        super().__init__(coordinator, "Case ID", "case_id")


class ArgusStatusSensor(ArgusSensorBase):
    """Lifecycle status of the active case."""

    _data_key = "case_status"
    _attr_icon = "mdi:state-machine"

    def __init__(self, coordinator):
        super().__init__(coordinator, "Status", "status")
