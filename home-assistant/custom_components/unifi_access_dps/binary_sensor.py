"""Door position + lock relay binary sensors fed from UniFi Access."""

import logging
from typing import Any

from homeassistant.components.binary_sensor import (
    BinarySensorDeviceClass,
    BinarySensorEntity,
)
from homeassistant.const import CONF_NAME
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.restore_state import RestoreEntity

from .const import CONF_DOOR_ID, DOMAIN, SIGNAL_UPDATE

_LOGGER = logging.getLogger(__name__)


async def async_setup_platform(
    hass: HomeAssistant, config, async_add_entities, discovery_info=None
) -> None:
    """Set up binary sensors from the hub created in async_setup."""
    if discovery_info is None:
        return
    hub = hass.data[DOMAIN]
    entities: list[BinarySensorEntity] = []
    for device_id, door in hub.doors.items():
        entities.append(UnifiAccessDpsBinarySensor(hub, device_id, door[CONF_NAME]))
        if door.get(CONF_DOOR_ID):
            entities.append(
                UnifiAccessLockBinarySensor(hub, device_id, door[CONF_NAME])
            )
    async_add_entities(entities)


class UnifiAccessBaseBinarySensor(RestoreEntity, BinarySensorEntity):
    """Shared push-update plumbing for the door sensors."""

    _attr_should_poll = False

    def __init__(self, hub, device_id: str) -> None:
        self._hub = hub
        self._device_id = device_id
        self._restored: bool | None = None

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        if (last := await self.async_get_last_state()) is not None and last.state in (
            "on",
            "off",
        ):
            self._restored = last.state == "on"
        self.async_on_remove(
            async_dispatcher_connect(self.hass, SIGNAL_UPDATE, self._handle_update)
        )

    @callback
    def _handle_update(self) -> None:
        self.async_write_ha_state()

    @property
    def _state(self) -> dict[str, Any]:
        return self._hub.states[self._device_id]


class UnifiAccessDpsBinarySensor(UnifiAccessBaseBinarySensor):
    """Door position from the hub's raw DPS input (on = open).

    The input is a closed-circuit-when-closed contact: input value "on" means the
    door is closed, "off" means open. Until the first live device.update arrives
    (they are only emitted when hub state changes), the last known state is restored
    from the HA database.
    """

    _attr_device_class = BinarySensorDeviceClass.DOOR

    def __init__(self, hub, device_id: str, name: str) -> None:
        super().__init__(hub, device_id)
        self._attr_name = f"{name} Position"
        self._attr_unique_id = f"{DOMAIN}_{device_id}"

    @property
    def is_on(self) -> bool | None:
        value = self._state["value"]
        if value is None:
            return self._restored
        return value == "off"

    @property
    def available(self) -> bool:
        # While connected but before the first device.update, show the restored
        # state (or "unknown" on a fresh install) rather than "unavailable".
        return self._state["value"] is not None or self._hub.connected

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        return {
            "raw_input": self._state["value"],
            "input_updated_at": self._state["updated_at"],
            "wiring_detected": self._state["wiring_detected"],
            "ws_connected": self._hub.connected,
            "restored": self._state["value"] is None and self._restored is not None,
        }


class UnifiAccessLockBinarySensor(UnifiAccessBaseBinarySensor):
    """Maglock relay state (on = unlocked, per HA lock device-class convention).

    Fed by websocket location updates plus a periodic doors poll (which also seeds
    the state at startup).
    """

    _attr_device_class = BinarySensorDeviceClass.LOCK

    def __init__(self, hub, device_id: str, name: str) -> None:
        super().__init__(hub, device_id)
        self._attr_name = f"{name} Lock"
        self._attr_unique_id = f"{DOMAIN}_{device_id}_lock"

    @property
    def is_on(self) -> bool | None:
        lock = self._state["lock"]
        if lock is None:
            return self._restored
        return lock == "unlocked"

    @property
    def available(self) -> bool:
        return self._state["lock"] is not None or self._hub.connected

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        return {
            "lock_state": self._state["lock"],
            "lock_updated_at": self._state["lock_updated_at"],
            "ws_connected": self._hub.connected,
        }
