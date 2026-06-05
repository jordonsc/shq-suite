"""Somfy SDN integration for Home Assistant.

Talks to the somfy-sdn ESP32 over WebSockets (ws_api.cpp). The firmware emits authoritative
per-motor state; we render it without optimistic / re-send machinery — the firmware owns write
reliability and reports what motors confirm via polling. One `cover` entity per motor; the
calibration controls are exposed as services (see services.yaml).
"""

import logging

from homeassistant.config_entries import ConfigEntry
from homeassistant.const import Platform
from homeassistant.core import HomeAssistant
from homeassistant.helpers.device_registry import DeviceEntry

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator

_LOGGER = logging.getLogger(__name__)

PLATFORMS = [
    Platform.COVER,
    Platform.BUTTON,
    Platform.SWITCH,
    Platform.NUMBER,
    Platform.SENSOR,
    Platform.BINARY_SENSOR,
]


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Set up from a config entry."""
    coordinator = SomfySdnCoordinator(hass, entry)
    await coordinator.async_start()

    hass.data.setdefault(DOMAIN, {})[entry.entry_id] = coordinator

    await hass.config_entries.async_forward_entry_setups(entry, PLATFORMS)
    return True


async def async_remove_config_entry_device(
    hass: HomeAssistant, config_entry: ConfigEntry, device_entry: DeviceEntry
) -> bool:
    """Allow deleting a motor device from the UI (e.g. a swapped-out / stale motor).

    Extracts the motor addr from the device identifier ("<host>:<port>:AA:BB:CC") and tells the
    firmware to forget it so it doesn't re-appear. The controller (hub) device can't be deleted
    this way (delete the integration instead).
    """
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][config_entry.entry_id]
    addr: str | None = None
    for domain, ident in device_entry.identifiers:
        if domain != DOMAIN:
            continue
        parts = ident.split(":")
        if len(parts) >= 5:  # host, port, AA, BB, CC
            addr = ":".join(parts[-3:])
    if addr is None:
        return False  # hub device — not removable while configured

    try:
        await coordinator.async_send_command("forget", addr=addr)
    except Exception as err:  # noqa: BLE001
        _LOGGER.debug("forget %s failed (removing from HA anyway): %s", addr, err)
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Unload a config entry."""
    unload_ok = await hass.config_entries.async_unload_platforms(entry, PLATFORMS)
    if unload_ok:
        coordinator: SomfySdnCoordinator = hass.data[DOMAIN].pop(entry.entry_id)
        await coordinator.async_shutdown()
    return unload_ok
