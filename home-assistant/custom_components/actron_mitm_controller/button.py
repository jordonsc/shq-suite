"""Reboot and Reconnect-WiFi buttons for the Actron MITM bridge.

Deliberately manual, not automatic. A reboot clears any clock fault instantly, and it was tempting
to make the firmware self-heal that way — but a reboot also destroys the RAM-only diagnostic ring,
which is the only record of what went wrong. The somfy twin's nine-hour clock wedge was diagnosed
precisely because nobody rebooted it (ledger shq-suite-0041). The firmware now recovers from a
clock fault on its own; this is for the cases it cannot.

A reboot is NOT free: the bridge sits on a physically cut RS485 bus, so while the ESP32 restarts
the NEO<->indoor-board link is severed for ~8-30 s and the indoor unit keeps running with no
controller (ledger shq-suite-0042). Turn every zone off first.

Reconnect WiFi (component 1.10.0, firmware >= 1.14.4) drops and re-scans the STA link for the
strongest AP. WiFi only — the RS485 relay is untouched — so it carries none of that cost, and it is
the first thing to try on a link that has gone bad. Older firmware answers it with an error.
"""

from homeassistant.components.button import ButtonEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
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
    async_add_entities(
        [ActronRebootButton(coordinator, entry), ActronReconnectWifiButton(coordinator, entry)]
    )


class ActronRebootButton(ButtonEntity):
    _attr_has_entity_name = True
    _attr_should_poll = False
    _attr_name = "Reboot"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:restart"

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        self.coordinator = coordinator
        self._attr_unique_id = f"{entry.entry_id}_reboot"
        self._attr_device_info = DeviceInfo(
            identifiers={(DOMAIN, entry.entry_id)},
            name="Actron AC",
            manufacturer="Actron",
            model="NEO via local MITM bridge",
            configuration_url=f"http://{coordinator.host}/",
        )

    async def async_press(self) -> None:
        await self.coordinator.async_send_command("reboot")


class ActronReconnectWifiButton(ButtonEntity):
    _attr_has_entity_name = True
    _attr_should_poll = False
    _attr_name = "Reconnect WiFi"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:wifi-sync"

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        self.coordinator = coordinator
        self._attr_unique_id = f"{entry.entry_id}_reconnect_wifi"
        self._attr_device_info = DeviceInfo(identifiers={(DOMAIN, entry.entry_id)})

    async def async_press(self) -> None:
        await self.coordinator.async_send_command("reconnect_wifi")
