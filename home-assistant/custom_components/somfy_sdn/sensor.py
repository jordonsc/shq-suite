"""Diagnostic sensors for Somfy SDN (controller device).

Two groups: the original bus-level pair (motors online, wire errors), and the firmware
self-diagnostics fed by the 30 s `health` push (ledger shq-suite-0038, ported from
actron_mitm_controller 1.2.0).

The health-fed ones deliberately DO NOT gate on `coordinator.is_available()` — their whole purpose
is to describe the run-up to a dropout, and blanking them the moment the link goes down would erase
exactly the evidence they exist to keep. Their last known value stands until a newer one arrives.
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
from homeassistant.helpers.dispatcher import async_dispatcher_connect
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnControllerEntity


@dataclass(frozen=True, kw_only=True)
class SomfyDiagSensorDescription(SensorEntityDescription):
    """A health-payload key plus how to render it."""

    value_fn: Callable[[dict[str, Any]], Any]


DIAG_SENSORS: tuple[SomfyDiagSensorDescription, ...] = (
    SomfyDiagSensorDescription(
        key="free_heap", name="Free heap",
        native_unit_of_measurement=UnitOfInformation.BYTES,
        device_class=SensorDeviceClass.DATA_SIZE,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("heap"),
    ),
    SomfyDiagSensorDescription(
        key="min_free_heap", name="Minimum free heap",
        native_unit_of_measurement=UnitOfInformation.BYTES,
        device_class=SensorDeviceClass.DATA_SIZE,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("min_heap"),
    ),
    SomfyDiagSensorDescription(
        key="largest_free_block", name="Largest free block",
        native_unit_of_measurement=UnitOfInformation.BYTES,
        device_class=SensorDeviceClass.DATA_SIZE,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Well below free heap means fragmentation: plenty of memory, none of it usable in one
        # piece, which is how an allocation for a new socket fails on a device that looks healthy.
        value_fn=lambda h: h.get("max_block"),
    ),
    SomfyDiagSensorDescription(
        key="spare_sockets", name="Spare sockets",
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Measured by asking lwIP for sockets until it refuses. 0 means the pool HTTP, WS, OTA and
        # mDNS all share is exhausted — the direct test of the socket-starvation hypothesis.
        value_fn=lambda h: h.get("spare_sockets"),
    ),
    SomfyDiagSensorDescription(
        key="ws_clients", name="WebSocket clients",
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("clients"),
    ),
    SomfyDiagSensorDescription(
        key="ws_connects", name="WebSocket connects",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("ws_conn"),
    ),
    SomfyDiagSensorDescription(
        key="ws_disconnects", name="WebSocket disconnects",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("ws_disc"),
    ),
    SomfyDiagSensorDescription(
        key="pong_timeouts", name="Pong timeouts",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Clients the FIRMWARE evicted for missing the ping/pong deadline. If this tracks the
        # unavailable count, the device is dropping a healthy HA — check the loop-stall sensor
        # next for why the pong was late.
        value_fn=lambda h: h.get("pong_timeouts"),
    ),
    SomfyDiagSensorDescription(
        key="skipped_writes", name="Skipped writes",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Frames the write-guard declined because the socket would have blocked.
        # Paired with the reap count this grades the fault: skips that recover mean a
        # briefly busy peer, skips ending in a reap mean the path died.
        value_fn=lambda h: h.get("skipped_writes"),
    ),
    SomfyDiagSensorDescription(
        key="bssid", name="Access point (BSSID)",
        entity_category=EntityCategory.DIAGNOSTIC,
        # Which radio is actually serving this device, read from the STATION — the
        # only authoritative source (the UniFi controller client list has been seen
        # disagreeing with it). In history this is what separates an AP fault from a
        # device fault: flaps that follow one BSSID across devices are the AP.
        value_fn=lambda h: h.get("bssid"),
    ),
    SomfyDiagSensorDescription(
        key="wifi_roams", name="AP roams",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Lifetime BSSID changes. Distinguishes "parked on the bad AP throughout"
        # from "the flap coincided with a roam".
        value_fn=lambda h: h.get("wifi_roams"),
    ),
    SomfyDiagSensorDescription(
        key="stall_reaps", name="Stalled sockets reaped",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Sockets the firmware's write-guard dropped for staying unwritable. Each one is a
        # ~10 s main-loop stall that did NOT happen: writing to a blocked socket costs
        # WIFI_CLIENT_MAX_WRITE_RETRY x WIFI_CLIENT_SELECT_TIMEOUT_US inside the Arduino
        # core, which no timeout above it can shorten (ledger shq-suite-0038). A climbing
        # count with a FLAT worst-loop-stall is the fix working, not a fault.
        value_fn=lambda h: h.get("stall_reaps"),
    ),
    SomfyDiagSensorDescription(
        key="peer_closes", name="Peer closes",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("peer_closes"),
    ),
    SomfyDiagSensorDescription(
        key="transport_errors", name="Transport errors",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("transport_errors"),
    ),
    SomfyDiagSensorDescription(
        key="loop_max_ms", name="Worst loop stall",
        native_unit_of_measurement=UnitOfTime.MILLISECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Since boot. Anything approaching the 5 s pong deadline explains an eviction outright.
        value_fn=lambda h: h.get("loop_max_ms"),
    ),
    SomfyDiagSensorDescription(
        key="http_max_ms", name="Worst HTTP stall",
        native_unit_of_measurement=UnitOfTime.MILLISECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # The HTTP pump blocks the whole main loop while it waits on a slow request, so this is
        # how much of a loop stall a passing HTTP client is responsible for.
        value_fn=lambda h: h.get("http_max_ms"),
    ),
    SomfyDiagSensorDescription(
        key="loop_stalls", name="Loop stalls",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("loop_stalls"),
    ),
    SomfyDiagSensorDescription(
        key="wifi_disconnects_diag", name="WiFi disconnects",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("wifi_disc"),
    ),
    SomfyDiagSensorDescription(
        key="heartbeat_age", name="Heartbeat age",
        native_unit_of_measurement=UnitOfTime.MILLISECONDS,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        # The shq-suite-0034 wedge indicator: climbing without bound means the state push has
        # stalled even though the socket is fine.
        value_fn=lambda h: h.get("hb_age_ms"),
    ),
    SomfyDiagSensorDescription(
        key="rssi", name="Signal strength",
        native_unit_of_measurement="dBm",
        device_class=SensorDeviceClass.SIGNAL_STRENGTH,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("rssi"),
    ),
    SomfyDiagSensorDescription(
        key="uptime", name="Uptime",
        native_unit_of_measurement=UnitOfTime.SECONDS,
        device_class=SensorDeviceClass.DURATION,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("uptime_s"),
    ),
)


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    entities: list[SensorEntity] = [
        MotorsOnlineSensor(coordinator),
        RecentErrorsSensor(coordinator),
        LastDisconnectSensor(coordinator),
    ]
    entities += [SomfyDiagSensor(coordinator, desc) for desc in DIAG_SENSORS]
    async_add_entities(entities)


class MotorsOnlineSensor(SomfySdnControllerEntity, SensorEntity):
    _attr_name = "Motors online"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_state_class = SensorStateClass.MEASUREMENT
    _attr_icon = "mdi:check-network"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:motors_online"

    @property
    def native_value(self) -> int:
        devs = (self.coordinator.data or {}).get("devices", [])
        return sum(1 for d in devs if d.get("online"))


class RecentErrorsSensor(SomfySdnControllerEntity, SensorEntity):
    _attr_name = "Wire errors"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_state_class = SensorStateClass.TOTAL_INCREASING
    _attr_icon = "mdi:alert-circle"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:errors"

    @property
    def native_value(self) -> int | None:
        return (self.coordinator.data or {}).get("errors_recent")


class _HealthFedSensor(SomfySdnControllerEntity, SensorEntity):
    """Controller-device sensor driven by the firmware `health` push, not by coordinator state."""

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)

    @property
    def available(self) -> bool:  # type: ignore[override]
        # Overrides SomfySdnControllerEntity, which gates on coordinator availability. These
        # sensors describe the run-up to a dropout; going unavailable alongside the link would
        # discard the evidence at precisely the moment it becomes interesting.
        return True

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
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


class SomfyDiagSensor(_HealthFedSensor):
    """One scalar from the firmware health payload."""

    entity_description: SomfyDiagSensorDescription

    def __init__(
        self, coordinator: SomfySdnCoordinator, description: SomfyDiagSensorDescription
    ) -> None:
        super().__init__(coordinator)
        self.entity_description = description
        self._attr_unique_id = f"{self._cid}:{description.key}"

    @property
    def native_value(self) -> Any:
        health = self.coordinator.health
        if not health:
            return None
        return self.entity_description.value_fn(health)


class LastDisconnectSensor(_HealthFedSensor):
    """The reason the firmware gave for the most recent WS disconnect.

    Arrives as an event rather than a periodic sample, so putting it on an entity gives the
    recorder a timeline of causes rather than just a disconnect count.
    """

    _attr_name = "Last disconnect reason"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_icon = "mdi:lan-disconnect"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:last_disconnect_reason"

    async def async_added_to_hass(self) -> None:
        await super().async_added_to_hass()
        self.async_on_remove(
            async_dispatcher_connect(
                self.hass,
                f"{DOMAIN}_disconnect_{self.coordinator.entry_id}",
                self._refresh,
            )
        )

    @property
    def native_value(self) -> Optional[str]:
        record = self.coordinator.last_disconnect
        return record.get("reason") if record else None

    @property
    def extra_state_attributes(self) -> Optional[dict[str, Any]]:
        # The evidence behind the classification, so a wrong call can be re-judged from history
        # without reflashing the fleet.
        return self.coordinator.last_disconnect or None
