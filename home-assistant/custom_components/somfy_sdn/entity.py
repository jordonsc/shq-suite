"""Shared base entities + dynamic per-motor setup for Somfy SDN.

Two device types: a single *controller* device per ESP32 (bus-level controls + diagnostics) and
one *motor* device per discovered cover (the shade + its calibration entities). The motor device
identifiers/`via_device` are kept identical to those `cover.py` produces so every entity for a
motor lands on the same HA device.
"""

from __future__ import annotations

from collections.abc import Callable, Iterable

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import callback
from homeassistant.helpers.entity import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator


def controller_id(coordinator: SomfySdnCoordinator) -> str:
    """Stable identifier for the controller device (one per config entry / ESP32).

    The MAC (bare hex) when known, else host:port. MAC-keyed so entity unique_ids and device
    identifiers survive a DHCP IP change (see async_migrate_entry for the v1->v2 rename).
    """
    return coordinator.controller_key


class SomfySdnControllerEntity(CoordinatorEntity[SomfySdnCoordinator]):
    """Base for bus-level entities, attached to the controller device."""

    _attr_has_entity_name = True

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._cid = controller_id(coordinator)

    @property
    def device_info(self) -> DeviceInfo:
        # Name off the MAC (stable across DHCP changes), falling back to the host until the MAC
        # is known. `configuration_url` gives HA a "Visit" link to the device's web dashboard;
        # it tracks the current (self-healed) IP.
        ident = self.coordinator.mac or self.coordinator.host
        return DeviceInfo(
            identifiers={(DOMAIN, self._cid)},
            name=f"Somfy SDN ({ident})",
            manufacturer="SHQ",
            model="Somfy SDN controller",
            configuration_url=f"http://{self.coordinator.host}/",
        )

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()


class SomfySdnMotorEntity(CoordinatorEntity[SomfySdnCoordinator]):
    """Base for per-motor entities, attached to that motor's device."""

    _attr_has_entity_name = True

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str) -> None:
        super().__init__(coordinator)
        self._addr = addr
        self._cid = controller_id(coordinator)

    @property
    def _device(self) -> dict:
        for d in (self.coordinator.data or {}).get("devices", []):
            if d.get("addr") == self._addr:
                return d
        return {}

    @property
    def device_info(self) -> DeviceInfo:
        label = self._device.get("label") or f"Somfy {self._addr}"
        return DeviceInfo(
            identifiers={(DOMAIN, f"{self._cid}:{self._addr}")},
            name=label,
            manufacturer="Somfy",
            model="SDN motor",
            via_device=(DOMAIN, self._cid),
        )

    @property
    def available(self) -> bool:
        return self.coordinator.is_available() and bool(self._device.get("online", False))


def add_motor_entities(
    coordinator: SomfySdnCoordinator,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
    factory: Callable[[str], Iterable],
) -> None:
    """Create entities for each motor as it appears in the firmware state, once per addr."""
    known: set[str] = set()

    @callback
    def _discover() -> None:
        new = []
        for dev in (coordinator.data or {}).get("devices", []):
            addr = dev.get("addr")
            if addr and addr not in known:
                known.add(addr)
                new.extend(factory(addr))
        if new:
            async_add_entities(new)

    entry.async_on_unload(coordinator.async_add_listener(_discover))
    _discover()
