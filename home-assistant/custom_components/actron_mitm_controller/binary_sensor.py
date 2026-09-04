"""Controller-level problem binary sensor for the Actron MITM bridge.

`sensor.actron_ac_fault` carries WHICH fault and is the one to read; this exists because a
device_class PROBLEM boolean is what HA's automation UI, alert helpers and the device page all
understand. The two are the same signal at different resolutions (ledger shq-suite-0041).
"""

from typing import Optional

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import ActronMitmCoordinator


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    coordinator: ActronMitmCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([ActronProblemBinarySensor(coordinator, entry)])


class ActronProblemBinarySensor(BinarySensorEntity):
    """On whenever the bridge reports any fault."""

    _attr_has_entity_name = True
    _attr_should_poll = False
    _attr_name = "Problem"
    _attr_device_class = BinarySensorDeviceClass.PROBLEM
    _attr_entity_category = EntityCategory.DIAGNOSTIC

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        self.coordinator = coordinator
        self._attr_unique_id = f"{entry.entry_id}_problem"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, entry.entry_id)},
            name="Actron AC",
            manufacturer="Actron",
            model="NEO via local MITM bridge",
            configuration_url=f"http://{coordinator.host}/",
        )

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self.async_on_remove(
            async_dispatcher_connect(
                self.hass, f"{DOMAIN}_health_{self.coordinator.entry_id}", self._handle_health
            )
        )

    @callback
    def _handle_health(self) -> None:
        self.async_write_ha_state()

    @property
    def available(self) -> bool:
        # Health-fed: it does not blank when the link drops, because the last known fault is the
        # most useful thing to be showing at exactly that moment.
        return True

    @property
    def is_on(self) -> Optional[bool]:
        health = self.coordinator.health or {}
        code: Optional[str] = health.get("fault")
        # None (not False) when the firmware does not report faults: "we do not know" and
        # "there is no problem" must not look the same.
        if code is None:
            return None
        return code != "ok"
