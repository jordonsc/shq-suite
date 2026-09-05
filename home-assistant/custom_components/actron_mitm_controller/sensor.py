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
        # Clients the FIRMWARE evicted for silence: fw >= 1.14.0 means nothing inbound (pong,
        # ping or text) for 45 s, extended to 120 s while lwIP is still retransmitting; older
        # firmware meant a pong 25 s late. If this tracks the unavailable count, the device
        # is dropping a live HA — read `_last_disconnect_reason`'s tcp_* attributes next.
        value_fn=lambda h: h.get("pong_timeouts"),
    ),
    ActronDiagSensorDescription(
        key="deferred_reaps", name="Deferred reaps",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Judgements the firmware declined because an unwritable client was still inside
        # its 10 s minimum age (a coordinator mid-handshake looks dead at the socket
        # layer). Climbing here means young sockets ARE going unwritable — the gate is
        # doing real work. Since fw 1.14.0 the deadlines above it (30 s unwritable reap,
        # 45 s silence) imply this one; it is kept as an explicit invariant
        # (ledger shq-suite-0038 / 0046, firmware ws_liveness.h).
        value_fn=lambda h: h.get("deferred_reaps"),
    ),
    ActronDiagSensorDescription(
        key="skipped_writes", name="Skipped writes",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Frames the write-guard declined because the socket would have blocked.
        # Paired with the reap count this grades the fault: skips that recover mean a
        # briefly busy peer, skips ending in a reap mean the path died.
        value_fn=lambda h: h.get("skipped_writes"),
    ),
    ActronDiagSensorDescription(
        key="bssid", name="Access point (BSSID)",
        entity_category=EntityCategory.DIAGNOSTIC,
        # Which radio is actually serving this device, read from the STATION — the
        # only authoritative source (the UniFi controller client list has been seen
        # disagreeing with it). In history this is what separates an AP fault from a
        # device fault: flaps that follow one BSSID across devices are the AP.
        value_fn=lambda h: h.get("bssid"),
    ),
    ActronDiagSensorDescription(
        key="clock_backward_reads", name="Clock backward reads",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # READ THIS AS A RATE. On this firmware the healthy value is a true zero (bridgeTask
        # never touches the filtered clock), so ANY sustained climb is significant; thousands
        # per second means the monotonic clock is pinned right now and every deadline in the
        # firmware has stopped firing. That is what a nine-hour outage looked like on the somfy
        # twin (ledger shq-suite-0041). History on this entity makes the rate visible.
        value_fn=lambda h: h.get("clk_back"),
    ),
    ActronDiagSensorDescription(
        key="clock_word_steps", name="Clock high-word faults",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Clock steps that were an exact multiple of 2^32 microseconds — provably a corrupt
        # high word in the 64-bit microsecond counter, rejected on the first read.
        value_fn=lambda h: h.get("clk_word"),
    ),
    ActronDiagSensorDescription(
        key="clock_rebases", name="Clock re-baselines",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Times the monotonic clamp gave up and adopted the clock the device actually has.
        # Each one is an outage that DIDN'T happen.
        value_fn=lambda h: h.get("clk_rebase"),
    ),
    ActronDiagSensorDescription(
        key="wifi_roams", name="AP roams",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Lifetime BSSID changes. Distinguishes "parked on the bad AP throughout"
        # from "the flap coincided with a roam".
        value_fn=lambda h: h.get("wifi_roams"),
    ),
    ActronDiagSensorDescription(
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
        # Since boot. A multi-second stall is a fault in its own right; since fw 1.14.0 it can
        # no longer cost a session on its own (the liveness deadline is 45 s).
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
    # WiFi MAC-layer transmit counters (component 1.7.0, fw 1.12.0, txstats.{h,cpp} — twin of the
    # somfy_sdn set). Read-only instrumentation for the long-frame uplink-loss investigation on the
    # somfy fleet; carried here so the twins stay in step and this device is a control. Lifetime
    # totals since boot; a `tx_en` of 0 in the health payload means the driver never enabled them.
    ActronDiagSensorDescription(
        key="tx_success",
        name="WiFi frames acknowledged",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-arrow-up",
        value_fn=lambda h: h.get("tx_ok"),
    ),
    ActronDiagSensorDescription(
        key="tx_retries",
        name="WiFi TX retries",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-refresh",
        # EDCA + trigger-based retransmissions. Retries per acknowledged frame is the figure that
        # separates a device the AP cannot hear from one that is simply not transmitting much.
        value_fn=lambda h: h.get("tx_retry"),
    ),
    ActronDiagSensorDescription(
        key="tx_tb_retries",
        name="WiFi TB retries",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-refresh",
        # Retries inside 11ax trigger-based PPDUs — non-zero only on an HE association.
        value_fn=lambda h: h.get("tx_tbretry"),
    ),
    ActronDiagSensorDescription(
        key="tx_ack_timeouts",
        name="WiFi ACK timeouts",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-alert",
        # The frame went out and no ACK/Block-ACK ever came back.
        value_fn=lambda h: h.get("tx_to"),
    ),
    ActronDiagSensorDescription(
        key="tx_collisions",
        name="WiFi collisions",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-alert",
        value_fn=lambda h: h.get("tx_coll"),
    ),
    ActronDiagSensorDescription(
        key="tx_no_mem",
        name="WiFi TX buffer starvation",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:memory",
        # The driver could not allocate for a transmission — points at the device, not the air.
        value_fn=lambda h: h.get("tx_nomem"),
    ),
    ActronDiagSensorDescription(
        key="tx_failures",
        name="WiFi TX failures",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-off",
        # Entries in the driver's failure-state matrix (RTS/CTS/data/ACK stages).
        value_fn=lambda h: h.get("tx_fail"),
    ),
    ActronDiagSensorDescription(
        key="phy_mode",
        name="WiFi PHY mode",
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-settings",
        # "HE20 ch6 bw20 bgnax" — negotiated PHY mode, primary channel, bandwidth, AP PHY set.
        value_fn=lambda h: h.get("phy"),
    ),
    ActronDiagSensorDescription(
        key="wifi_protocol",
        name="WiFi protocol",
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-cog",
        # The fw 1.13.0 A/B knob as reported (`bgnax` default / `bgn` / `bg`), against `phy_mode`.
        value_fn=lambda h: h.get("proto"),
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
    entities.append(ActronFaultSensor(coordinator, entry))
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


class ActronFaultSensor(_DiagBase):
    """The bridge's own worst active fault, as a short slug ("ok" when clear).

    Deliberately a TEXT sensor rather than a boolean: a boolean cannot say WHICH fault, and
    "clock stalled" and "no RS485 frames" want very different responses. The slug is the state;
    the human sentence and the full active set are attributes. Twin of the somfy_sdn sensor —
    both firmwares share the fault registry (ledger shq-suite-0041).
    """

    _attr_name = "Fault"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_icon = "mdi:alert-circle-outline"

    def __init__(self, coordinator: ActronMitmCoordinator, entry: ConfigEntry):
        super().__init__(coordinator, entry)
        self._attr_unique_id = f"{entry.entry_id}_fault"

    @property
    def native_value(self) -> Optional[str]:
        health = self.coordinator.health
        if not health:
            return None
        # A MISSING key means firmware too old to report faults at all — that must read as
        # `unknown`, never as "ok". Mapping absence to "ok" would have this sensor cheerfully
        # declare a healthy device on exactly the firmware that cannot tell us otherwise, which
        # is the false all-clear the whole fault registry exists to abolish.
        return health.get("fault")

    @property
    def extra_state_attributes(self) -> Optional[dict[str, Any]]:
        health = self.coordinator.health
        if not health:
            return None
        return {
            "detail": health.get("fault_detail") or "",
            # Bitmask of every active code, not just the worst.
            "mask": health.get("fault_mask"),
        }


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
