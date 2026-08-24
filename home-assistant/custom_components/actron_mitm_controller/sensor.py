"""Diagnostic sensors fed by the firmware's `health` push (ledger shq-suite-0038).

These exist so the device's own vitals land in HA's recorder, where they can be plotted against
the `unavailable` flaps instead of being sampled by hand over HTTP — which is what the earlier
investigation had to do, and which perturbed the very fault it was measuring (polling /stats every
15 s quadrupled the flap rate).

Everything here is EntityCategory.DIAGNOSTIC: none of it belongs on a dashboard, all of it belongs
in history. The counters are TOTAL_INCREASING so a device reboot reads as a reset rather than a
cliff, and the sensors stay available while the WS link is down — their last known value is the
evidence, and blanking them on disconnect would erase the run-up to every fault.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable, Optional

from homeassistant.components.sensor import (
    SensorDeviceClass,
    SensorEntity,
    SensorEntityDescription,
    SensorStateClass,
)
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory, UnitOfInformation, UnitOfTime
from homeassistant.core import HomeAssistant, callback
from homeassistant.helpers.device_registry import DeviceInfo
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import ActronMitmCoordinator


@dataclass(frozen=True, kw_only=True)
class ActronDiagSensorDescription(SensorEntityDescription):
    """A health-payload key plus how to render it."""

    # Pulls the value out of the health dict. A plain key for most; a lambda where the useful
    # number is derived (e.g. heap fragmentation).
    value_fn: Callable[[dict[str, Any]], Any]


SENSORS: tuple[ActronDiagSensorDescription, ...] = (
    ActronDiagSensorDescription(
        key="free_heap",
        name="Free heap",
        native_unit_of_measurement=UnitOfInformation.BYTES,
        device_class=SensorDeviceClass.DATA_SIZE,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("heap"),
    ),
    ActronDiagSensorDescription(
        key="min_free_heap",
        name="Minimum free heap",
        native_unit_of_measurement=UnitOfInformation.BYTES,
        device_class=SensorDeviceClass.DATA_SIZE,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("min_heap"),
    ),
    ActronDiagSensorDescription(
        key="largest_free_block",
        name="Largest free block",
        native_unit_of_measurement=UnitOfInformation.BYTES,
        device_class=SensorDeviceClass.DATA_SIZE,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Well below free heap means fragmentation: plenty of memory, none of it usable in one
        # piece, which is how an allocation for a new socket fails on a device that looks healthy.
        value_fn=lambda h: h.get("max_block"),
    ),
    ActronDiagSensorDescription(
        key="spare_sockets",
        name="Spare sockets",
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Measured by asking lwIP for sockets until it refuses. 0 means the pool HTTP, WS, OTA and
        # mDNS all share is exhausted — the direct test of the socket-starvation hypothesis.
        value_fn=lambda h: h.get("spare_sockets"),
    ),
    ActronDiagSensorDescription(
        key="ws_clients",
        name="WebSocket clients",
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("clients"),
    ),
    ActronDiagSensorDescription(
        key="ws_connects",
        name="WebSocket connects",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("ws_conn"),
    ),
    ActronDiagSensorDescription(
        key="ws_disconnects",
        name="WebSocket disconnects",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("ws_disc"),
    ),
    ActronDiagSensorDescription(
        key="pong_timeouts",
        name="Pong timeouts",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Clients the FIRMWARE evicted for missing the ping/pong deadline. If this tracks the HA
        # unavailable count, the device is dropping a healthy HA — look at the loop-stall sensor
        # next for why the pong was late.
        value_fn=lambda h: h.get("pong_timeouts"),
    ),
    ActronDiagSensorDescription(
        key="peer_closes",
        name="Peer closes",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("peer_closes"),
    ),
    ActronDiagSensorDescription(
        key="transport_errors",
        name="Transport errors",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("transport_errors"),
    ),
    ActronDiagSensorDescription(
        key="loop_max_ms",
        name="Worst loop stall",
        native_unit_of_measurement=UnitOfTime.MILLISECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Since boot. Anything approaching the 5 s pong deadline explains an eviction outright.
        value_fn=lambda h: h.get("loop_max_ms"),
    ),
    ActronDiagSensorDescription(
        key="http_max_ms",
        name="Worst HTTP stall",
        native_unit_of_measurement=UnitOfTime.MILLISECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Arduino's WebServer::handleClient() blocks the whole main loop while it waits on a slow
        # request, so this is how much of the loop stall a passing HTTP client is responsible for.
        value_fn=lambda h: h.get("http_max_ms"),
    ),
    ActronDiagSensorDescription(
        key="loop_stalls",
        name="Loop stalls",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("loop_stalls"),
    ),
    ActronDiagSensorDescription(
        key="wifi_disconnects",
        name="WiFi disconnects",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("wifi_disc"),
    ),
    ActronDiagSensorDescription(
        key="heartbeat_age",
        name="Heartbeat age",
        native_unit_of_measurement=UnitOfTime.MILLISECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # The shq-suite-0034 wedge indicator: this climbing without bound means the state push has
        # stalled even though the socket is fine.
        value_fn=lambda h: h.get("hb_age_ms"),
    ),
    ActronDiagSensorDescription(
        key="rssi",
        name="Signal strength",
        native_unit_of_measurement="dBm",
        device_class=SensorDeviceClass.SIGNAL_STRENGTH,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("rssi"),
    ),
    ActronDiagSensorDescription(
        key="uptime",
        name="Uptime",
        native_unit_of_measurement=UnitOfTime.SECONDS,
        device_class=SensorDeviceClass.DURATION,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("uptime_s"),
    ),
)


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    coordinator: ActronMitmCoordinator = hass.data[DOMAIN][entry.entry_id]
    entities: list[SensorEntity] = [
        ActronDiagSensor(coordinator, entry, desc) for desc in SENSORS
    ]
    entities.append(ActronLastDisconnectSensor(coordinator, entry))
    async_add_entities(entities)


class _DiagBase(SensorEntity):
    """Shared device grouping and health-push subscription."""

    _attr_has_entity_name = True
    _attr_should_poll = False

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        self.coordinator = coordinator
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


class ActronDiagSensor(_DiagBase):
    """One scalar from the firmware health payload."""

    entity_description: ActronDiagSensorDescription

    def __init__(
        self,
        coordinator: ActronMitmCoordinator,
        entry: ConfigEntry,
        description: ActronDiagSensorDescription,
    ):
        super().__init__(coordinator, entry)
        self.entity_description = description
        self._attr_unique_id = f"{entry.entry_id}_{description.key}"

    @property
    def native_value(self) -> Any:
        health = self.coordinator.health
        if not health:
            return None
        return self.entity_description.value_fn(health)


class ActronLastDisconnectSensor(_DiagBase):
    """The reason the firmware gave for the most recent WS disconnect.

    Deliberately not derived from the health payload: it is the one piece of diagnostic state that
    arrives as an event rather than a periodic sample, and putting it on an entity means the
    recorder keeps a timeline of causes rather than a count.
    """

    _attr_name = "Last disconnect reason"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_icon = "mdi:lan-disconnect"

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        super().__init__(coordinator, entry)
        self._attr_unique_id = f"{entry.entry_id}_last_disconnect_reason"

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self.async_on_remove(
            async_dispatcher_connect(
                self.hass,
                f"{DOMAIN}_disconnect_{self.coordinator.entry_id}",
                self._handle_health,
            )
        )

    @property
    def native_value(self) -> Optional[str]:
        record = self.coordinator.last_disconnect
        return record.get("reason") if record else None

    @property
    def extra_state_attributes(self) -> Optional[dict[str, Any]]:
        # The evidence behind the classification, so a wrong call can be re-judged from history
        # without reflashing: socket lifetime, pong age, traffic counts, and the machine's
        # condition at that instant.
        return self.coordinator.last_disconnect or None
