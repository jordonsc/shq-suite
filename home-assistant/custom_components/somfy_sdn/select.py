"""Select entities for Somfy SDN.

Controller: WiFi protocol — the fw 1.13.0 A/B knob (ledger shq-suite-0046). Two of twelve
controllers lose long uplink frames in the air while their neighbours on the same radio do not,
and the ranked physical suspects all sit on the 11ax path the boards negotiate by default. This
lets one unit be moved off 11ax (`bgn`) or off 11n (`bg`) from HA, with its clean neighbour left
on `bgnax` as the control, and moved back the same way — no reflash, and the setting survives a
reboot (NVS). **Selecting an option REBOOTS the controller** (component 1.13.0 / fw 1.14.0):
`esp_wifi_set_protocol()` is honoured only on a fresh WiFi init — a live re-association left the
canary negotiating HE20 with `bgn` persisted, while a reboot came up HT20. The controller is gone
for ~10 s, the RAM diagnostic ring is lost, and the coordinator reconnects; the reason lands in
the next boot's `note=`.

`bgnax` is the untouched default: the firmware makes no driver call at all for it.
"""

from __future__ import annotations

from homeassistant.components.select import SelectEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnControllerEntity

WIFI_PROTO_OPTIONS = ["bgnax", "bgn", "bg"]


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([WifiProtocolSelect(coordinator)])


class WifiProtocolSelect(SomfySdnControllerEntity, SelectEntity):
    """Which 802.11 generations the station may negotiate (`proto` in the health push)."""

    _attr_name = "WiFi protocol"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:wifi-cog"
    _attr_options = WIFI_PROTO_OPTIONS

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:wifi_proto"

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        # The current value rides on the 30 s health push, not the state snapshot.
        self.async_on_remove(
            async_dispatcher_connect(
                self.hass,
                f"{DOMAIN}_health_{self.coordinator.entry_id}",
                self._refresh,
            )
        )

    @callback
    def _refresh(self) -> None:
        self.async_write_ha_state()

    @property
    def current_option(self) -> str | None:
        proto = (self.coordinator.health or {}).get("proto")
        return proto if proto in WIFI_PROTO_OPTIONS else None

    async def async_select_option(self, option: str) -> None:
        # ack = persisted; the firmware then REBOOTS to apply it (see the module docstring).
        # The first health push after reconnect carries the new value.
        await self.coordinator.async_send_command("set_wifi_proto", proto=option)
