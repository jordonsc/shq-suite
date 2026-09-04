"""Reboot button for the Actron MITM bridge.

Deliberately manual, not automatic. A reboot clears any clock fault instantly, and it was tempting
to make the firmware self-heal that way — but a reboot also destroys the RAM-only diagnostic ring,
which is the only record of what went wrong. The somfy twin's nine-hour clock wedge was diagnosed
precisely because nobody rebooted it (ledger shq-suite-0041). The firmware now recovers from a
clock fault on its own; this is for the cases it cannot.

The A/C itself is unaffected: the bridge fails safe to passthrough while the ESP32 is down, so a
restart costs a few seconds of relay, not a zone.
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
    async_add_entities([ActronRebootButton(coordinator, entry)])


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
