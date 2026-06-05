"""Cover platform for Somfy SDN motors.

One `cover.somfy_<addr>` per motor discovered in the firmware's state payload, created
dynamically as motors appear. Position is inverted at this boundary: the firmware reports NATIVE
Somfy % (0=open, 100=closed); HA wants 0=closed, 100=open, so
current_cover_position = 100 - somfy_pct (SPEC §5.5).

Calibration is also exposed as entity services here (for automations); the button/switch/number
platforms expose the same operations as dashboard-friendly entities.
"""

import logging
from typing import Any

import voluptuous as vol

from homeassistant.components.cover import (
    CoverDeviceClass,
    CoverEntity,
    CoverEntityFeature,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers import config_validation as cv
from homeassistant.helpers import entity_platform
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnMotorEntity, add_motor_entities

_LOGGER = logging.getLogger(__name__)


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    """Set up cover entities, adding new motors as the firmware reports them."""
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    add_motor_entities(
        coordinator, entry, async_add_entities, lambda addr: [SomfySdnCover(coordinator, addr)]
    )

    # Calibration + admin services (power-user; target entity_id). These mirror the button/
    # switch/number entities but are scriptable for automations.
    platform = entity_platform.async_get_current_platform()
    platform.async_register_entity_service(
        "move_steps",
        {
            vol.Required("direction"): vol.In(["up", "down"]),
            vol.Required("pulses"): vol.All(vol.Coerce(int), vol.Range(min=0, max=65535)),
        },
        "async_move_steps",
    )
    platform.async_register_entity_service("set_top_limit", {}, "async_set_top_limit")
    platform.async_register_entity_service("set_bottom_limit", {}, "async_set_bottom_limit")
    platform.async_register_entity_service(
        "set_bottom_limit_pulses",
        {vol.Required("pulses"): vol.All(vol.Coerce(int), vol.Range(min=0, max=65535))},
        "async_set_bottom_limit_pulses",
    )
    platform.async_register_entity_service(
        "set_direction", {vol.Required("reversed"): cv.boolean}, "async_set_direction"
    )
    platform.async_register_entity_service("reset", {}, "async_reset")
    platform.async_register_entity_service("identify", {}, "async_identify")
    platform.async_register_entity_service(
        "set_mode", {vol.Required("mode"): vol.In(["listen", "active"])}, "async_set_mode"
    )


class SomfySdnCover(SomfySdnMotorEntity, CoverEntity):
    """A single Somfy SDN roller shade motor."""

    _attr_device_class = CoverDeviceClass.SHADE
    _attr_supported_features = (
        CoverEntityFeature.OPEN
        | CoverEntityFeature.CLOSE
        | CoverEntityFeature.STOP
        | CoverEntityFeature.SET_POSITION
    )
    _attr_name = None  # the device name carries it

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str):
        super().__init__(coordinator, addr)
        self._attr_unique_id = f"{self._cid}:{addr}"

    # ---- cover state -----------------------------------------------------

    @property
    def current_cover_position(self) -> int | None:
        somfy = self._device.get("position")
        if somfy is None:
            return None
        return 100 - int(somfy)

    @property
    def is_closed(self) -> bool | None:
        somfy = self._device.get("position")
        if somfy is None:
            return None
        return int(somfy) >= 100

    @property
    def is_opening(self) -> bool:
        return self._device.get("moving") == "up"  # Somfy "up" = toward open

    @property
    def is_closing(self) -> bool:
        return self._device.get("moving") == "down"

    @property
    def extra_state_attributes(self) -> dict[str, Any]:
        d = self._device
        return {
            "address": self._addr,
            "status": d.get("status"),  # human-readable problem (e.g. "rejected: no limits") or "ok"
            "pulses": d.get("pulses"),  # absolute encoder count — handy for bulk provisioning
            "somfy_position": d.get("position"),
            "fault": d.get("fault"),
            "online": d.get("online"),
            "direction": d.get("direction"),
            "up_limit": d.get("up_limit"),
            "down_limit": d.get("down_limit"),
        }

    # ---- commands --------------------------------------------------------

    async def async_open_cover(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command("open", addr=self._addr)

    async def async_close_cover(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command("close", addr=self._addr)

    async def async_stop_cover(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command("stop", addr=self._addr)

    async def async_set_cover_position(self, **kwargs: Any) -> None:
        await self.coordinator.async_send_command(
            "set_position", addr=self._addr, position=kwargs["position"]
        )

    # ---- calibration / admin services ------------------------------------

    async def async_move_steps(self, direction: str, pulses: int) -> None:
        await self.coordinator.async_send_command(
            "move_steps", addr=self._addr, direction=direction, pulses=pulses
        )

    async def async_set_top_limit(self) -> None:
        await self.coordinator.async_send_command("set_top_limit", addr=self._addr)

    async def async_set_bottom_limit(self) -> None:
        await self.coordinator.async_send_command("set_bottom_limit", addr=self._addr)

    async def async_set_bottom_limit_pulses(self, pulses: int) -> None:
        await self.coordinator.async_send_command(
            "set_bottom_limit_pulses", addr=self._addr, pulses=pulses
        )

    async def async_set_direction(self, reversed: bool) -> None:  # noqa: A002
        await self.coordinator.async_send_command(
            "set_direction", addr=self._addr, reversed=reversed
        )

    async def async_reset(self) -> None:
        await self.coordinator.async_send_command("reset", addr=self._addr)

    async def async_identify(self) -> None:
        await self.coordinator.async_send_command("identify", addr=self._addr)

    async def async_set_mode(self, mode: str) -> None:
        await self.coordinator.async_send_command("set_mode", mode=mode)
