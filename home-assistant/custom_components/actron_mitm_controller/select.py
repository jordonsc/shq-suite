"""WiFi protocol select for the Actron MITM bridge — the fw 1.13.0 A/B knob.

Twin of the `somfy_sdn` select (ledger shq-suite-0046): which 802.11 generations the station may
negotiate — `bgnax` (the untouched default; the firmware makes no driver call), `bgn` (no 11ax)
or `bg` (no 11n). Persisted in NVS on the device. **Selecting an option REBOOTS the bridge**
(component 1.9.0 / fw 1.14.0): the protocol bitmap is honoured only on a fresh WiFi init, so the
1.13.0 re-associate did not apply it. That gives this the SAME A/C cost as the Reboot button —
the cut RS485 bus is severed for the restart — so turn every zone off first (shq-suite-0042).
"""

from __future__ import annotations

from homeassistant.components.select import SelectEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity import DeviceInfo
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import ActronMitmCoordinator

WIFI_PROTO_OPTIONS = ["bgnax", "bgn", "bg"]


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    coordinator: ActronMitmCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([ActronWifiProtocolSelect(coordinator, entry)])


class ActronWifiProtocolSelect(SelectEntity):
    _attr_has_entity_name = True
    _attr_should_poll = False
    _attr_name = "WiFi protocol"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:wifi-cog"
    _attr_options = WIFI_PROTO_OPTIONS

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        self.coordinator = coordinator
        self._attr_unique_id = f"{entry.entry_id}_wifi_proto"
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
                self.hass,
                f"{DOMAIN}_health_{self.coordinator.entry_id}",
                self._handle_health,
            )
        )

    @callback
    def _handle_health(self) -> None:
        self.async_write_ha_state()

    @property
    def available(self) -> bool:
        return self.coordinator.is_available()

    @property
    def current_option(self) -> str | None:
        proto = (self.coordinator.health or {}).get("proto")
        return proto if proto in WIFI_PROTO_OPTIONS else None

    async def async_select_option(self, option: str) -> None:
        await self.coordinator.async_send_command("set_wifi_proto", proto=option)
