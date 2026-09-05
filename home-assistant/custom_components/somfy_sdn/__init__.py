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
from homeassistant.helpers import device_registry as dr, entity_registry as er
from homeassistant.helpers.device_registry import DeviceEntry

from .const import CONF_HOST, CONF_PORT, DEFAULT_PORT, DOMAIN
from .coordinator import SomfySdnCoordinator, _mac_norm

_LOGGER = logging.getLogger(__name__)

PLATFORMS = [
    Platform.COVER,
    Platform.BUTTON,
    Platform.SWITCH,
    Platform.NUMBER,
    Platform.SENSOR,
    Platform.BINARY_SENSOR,
    Platform.SELECT,
]


async def async_migrate_entry(hass: HomeAssistant, entry: ConfigEntry) -> bool:
    """Migrate v1 (host:port-keyed) entries to v2 (MAC-keyed).

    The original entity unique_ids and device identifiers were built from "<host>:<port>", so a
    DHCP IP change re-keyed everything (orphan + recreate). v2 keys them on the controller MAC.
    Rewrites the existing registry entries in place so history / customisations (e.g. a renamed
    motor) survive, and renames the IP-bearing controller entity_ids to a MAC slug to match.
    Entries without a MAC unique_id (manual fallback) keep host:port — just bump the version.
    """
    if entry.version >= 2:
        return True

    mac = _mac_norm(entry.unique_id)
    if mac:
        host = entry.data[CONF_HOST]
        old_cid = f"{host}:{entry.data.get(CONF_PORT, DEFAULT_PORT)}"
        host_slug = host.replace(".", "_")
        mac_slug = "_".join(mac[i : i + 2] for i in range(0, 12, 2))

        ent_reg = er.async_get(hass)
        for e in er.async_entries_for_config_entry(ent_reg, entry.entry_id):
            updates: dict = {}
            if e.unique_id.startswith(old_cid):
                updates["new_unique_id"] = mac + e.unique_id[len(old_cid) :]
            if host_slug in e.entity_id:
                candidate = e.entity_id.replace(host_slug, mac_slug)
                if candidate != e.entity_id and ent_reg.async_get(candidate) is None:
                    updates["new_entity_id"] = candidate
            if updates:
                _LOGGER.info("migrate %s -> %s", e.entity_id, updates)
                ent_reg.async_update_entity(e.entity_id, **updates)

        dev_reg = dr.async_get(hass)
        for d in dr.async_entries_for_config_entry(dev_reg, entry.entry_id):
            new_ids = set()
            changed = False
            for domain, ident in d.identifiers:
                if domain == DOMAIN and ident.startswith(old_cid):
                    new_ids.add((domain, mac + ident[len(old_cid) :]))
                    changed = True
                else:
                    new_ids.add((domain, ident))
            if changed:
                dev_reg.async_update_device(d.id, new_identifiers=new_ids)
    else:
        _LOGGER.warning(
            "somfy_sdn entry %s has no MAC unique_id; keeping host:port identifiers", entry.title
        )

    hass.config_entries.async_update_entry(entry, version=2)
    return True


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

    Motor identifier = "<controller_key>:AA:BB:CC"; the controller (hub) identifier is just
    "<controller_key>". Extracts the motor addr and tells the firmware to forget it so it doesn't
    re-appear. The hub device can't be deleted this way (delete the integration instead).
    """
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][config_entry.entry_id]
    cid = coordinator.controller_key
    addr: str | None = None
    for domain, ident in device_entry.identifiers:
        if domain == DOMAIN and ident.startswith(cid + ":"):
            addr = ident[len(cid) + 1 :]
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
