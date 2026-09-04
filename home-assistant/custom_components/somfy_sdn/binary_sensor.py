"""Fault binary sensors for Somfy SDN: one per motor, plus one for the controller."""

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnControllerEntity, SomfySdnMotorEntity, add_motor_entities


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([ControllerProblemBinarySensor(coordinator)])
    add_motor_entities(
        coordinator, entry, async_add_entities, lambda addr: [FaultBinarySensor(coordinator, addr)]
    )


class FaultBinarySensor(SomfySdnMotorEntity, BinarySensorEntity):
    _attr_name = "Fault"
    _attr_device_class = BinarySensorDeviceClass.PROBLEM
    _attr_entity_category = EntityCategory.DIAGNOSTIC

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str) -> None:
        super().__init__(coordinator, addr)
        self._attr_unique_id = f"{self._cid}:{addr}:fault"

    @property
    def is_on(self) -> bool:
        return bool(self._device.get("fault"))


class ControllerProblemBinarySensor(SomfySdnControllerEntity, BinarySensorEntity):
    """On whenever the controller reports any fault. The boolean automations can trigger on.

    `sensor.<controller>_fault` carries WHICH fault and is the one to read; this exists because a
    device_class PROBLEM boolean is what HA's automation UI, alert helpers and the device page all
    understand. The two are the same signal at different resolutions.
    """

    _attr_name = "Problem"
    _attr_device_class = BinarySensorDeviceClass.PROBLEM
    _attr_entity_category = EntityCategory.DIAGNOSTIC

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:problem"

    @property
    def available(self) -> bool:  # type: ignore[override]
        # Health-fed, so it does not blank when the link drops — the last known fault is the most
        # useful thing to be showing at exactly that moment.
        return True

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self.async_on_remove(
            async_dispatcher_connect(
                self.hass, f"{DOMAIN}_health_{self.coordinator.entry_id}", self._refresh
            )
        )

    @callback
    def _refresh(self) -> None:
        self.async_write_ha_state()

    @property
    def is_on(self) -> Optional[bool]:
        health = self.coordinator.health or {}
        code = health.get("fault")
        # None (not False) when the firmware does not report faults: "we do not know" and
        # "there is no problem" must not look the same.
        if code is None:
            return None
        return code != "ok"
