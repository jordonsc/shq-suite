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
        # Clients the FIRMWARE evicted for silence: fw >= 1.14.0 means nothing inbound (pong,
        # ping or text) for 45 s, extended to 120 s while lwIP is still retransmitting; older
        # firmware meant a pong 25 s late. If this tracks the unavailable count, the device
        # is dropping a live HA — read `_last_disconnect_reason`'s tcp_* attributes next.
        value_fn=lambda h: h.get("pong_timeouts"),
    ),
    SomfyDiagSensorDescription(
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
        key="clock_backward_reads", name="Clock backward reads",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # READ THIS AS A RATE. A healthy controller gathers a few hundred over four days;
        # thousands per second means the monotonic clock is pinned RIGHT NOW and every
        # deadline in the firmware has stopped firing. That is the fingerprint of the
        # nine-hour Bed 2 outage, and it was sitting in plain sight the whole time —
        # a prior investigation wrote it off as a race artifact (ledger shq-suite-0039,
        # since corrected by shq-suite-0041). History on this entity makes the rate visible.
        value_fn=lambda h: h.get("clk_back"),
    ),
    SomfyDiagSensorDescription(
        key="clock_word_steps", name="Clock high-word faults",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Clock steps that were an exact multiple of 2^32 microseconds — provably a corrupt
        # high word in the 64-bit microsecond counter, rejected on the first read. Non-zero
        # means the hardware clock misbehaved and the firmware absorbed it.
        value_fn=lambda h: h.get("clk_word"),
    ),
    SomfyDiagSensorDescription(
        key="clock_rebases", name="Clock re-baselines",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        # Times the monotonic clamp gave up and adopted the clock the device actually has,
        # because it had stayed moved. Each one is an outage that DIDN'T happen: the old
        # firmware would have frozen here for as long as the step was large.
        value_fn=lambda h: h.get("clk_rebase"),
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
        # Since boot. A multi-second stall is a fault in its own right; since fw 1.14.0 it can
        # no longer cost a session on its own (the liveness deadline is 45 s).
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
    # Network-stack watchdog (fw 1.11.0, ledger shq-suite-0044). Probe failures climbing while
    # the link is up is the "associated but unreachable" signature that killed Bed 2; recoveries
    # counts the re-associations the firmware performed on its own to get out of it.
    SomfyDiagSensorDescription(
        key="gateway_probe_failures", name="Gateway probe failures",
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:lan-disconnect",
        value_fn=lambda h: h.get("nw_fail"),
    ),
    SomfyDiagSensorDescription(
        key="network_recoveries", name="Network recoveries",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-refresh",
        value_fn=lambda h: h.get("nw_recover"),
    ),
    SomfyDiagSensorDescription(
        key="uptime", name="Uptime",
        native_unit_of_measurement=UnitOfTime.SECONDS,
        device_class=SensorDeviceClass.DURATION,
        state_class=SensorStateClass.MEASUREMENT,
        entity_category=EntityCategory.DIAGNOSTIC,
        value_fn=lambda h: h.get("uptime_s"),
    ),
    # WiFi MAC-layer transmit counters (component 1.11.0, fw 1.12.0, txstats.{h,cpp}). Read-only
    # instrumentation for the long-frame uplink-loss investigation: two controllers on one AP
    # radio churn their WS session while ten identical neighbours do not, and a capture put the
    # loss in the uplink as a function of frame length. These say whether the WiFi driver is
    # retrying those frames to exhaustion — the one layer nothing had measured. Lifetime totals
    # since boot; the ratio of retries/timeouts to successes is the number to plot, and a
    # `tx_en` of 0 in the health payload means the driver never enabled them (all zeros then).
    SomfyDiagSensorDescription(
        key="tx_success", name="WiFi frames acknowledged",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-arrow-up",
        value_fn=lambda h: h.get("tx_ok"),
    ),
    SomfyDiagSensorDescription(
        key="tx_retries", name="WiFi TX retries",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-refresh",
        # EDCA + trigger-based retransmissions. Retries per acknowledged frame is the figure that
        # separates a device the AP cannot hear from one that is simply not transmitting much.
        value_fn=lambda h: h.get("tx_retry"),
    ),
    SomfyDiagSensorDescription(
        key="tx_tb_retries", name="WiFi TB retries",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-refresh",
        # Retries inside 11ax trigger-based PPDUs specifically — non-zero only on an HE
        # association, and the first place to look if the ax uplink path is the suspect.
        value_fn=lambda h: h.get("tx_tbretry"),
    ),
    SomfyDiagSensorDescription(
        key="tx_ack_timeouts", name="WiFi ACK timeouts",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-alert",
        # The frame went out and no ACK/Block-ACK ever came back: the AP did not hear it, or its
        # reply did not reach us. Climbing with the WS churn is the uplink dying on the air.
        value_fn=lambda h: h.get("tx_to"),
    ),
    SomfyDiagSensorDescription(
        key="tx_collisions", name="WiFi collisions",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-alert",
        value_fn=lambda h: h.get("tx_coll"),
    ),
    SomfyDiagSensorDescription(
        key="tx_no_mem", name="WiFi TX buffer starvation",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:memory",
        # The driver could not allocate for a transmission. Non-zero points at the device, not
        # the air — and it is the one counter here a queue of retried long frames would move.
        value_fn=lambda h: h.get("tx_nomem"),
    ),
    SomfyDiagSensorDescription(
        key="tx_failures", name="WiFi TX failures",
        state_class=SensorStateClass.TOTAL_INCREASING,
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-off",
        # Entries in the driver's failure-state matrix (RTS/CTS/data/ACK stages).
        value_fn=lambda h: h.get("tx_fail"),
    ),
    SomfyDiagSensorDescription(
        key="phy_mode", name="WiFi PHY mode",
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-settings",
        # "HE20 ch6 bw20 bgnax" — negotiated PHY mode, primary channel, bandwidth and the AP's
        # advertised PHY set. Whether the churning controllers negotiated 11ax (HE) where the
        # clean ones did not is the cheapest discriminator the capture could not see.
        value_fn=lambda h: h.get("phy"),
    ),
    SomfyDiagSensorDescription(
        key="wifi_protocol", name="WiFi protocol",
        entity_category=EntityCategory.DIAGNOSTIC,
        icon="mdi:wifi-cog",
        # The fw 1.13.0 A/B knob as the device reports it: what the station was ALLOWED to
        # negotiate (`bgnax` default / `bgn` / `bg`), against `phy_mode` above, which is what it
        # DID negotiate. Recorded so the churn history can be split by protocol setting.
        value_fn=lambda h: h.get("proto"),
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
    entities.append(FaultSensor(coordinator))
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


class FaultSensor(_HealthFedSensor):
    """The controller's own worst active fault, as a short slug ("ok" when clear).

    Deliberately a TEXT sensor rather than a boolean. When Bed 2's clock wedged for nine hours
    every signal the estate had was clear: the only fault entity was `binary_sensor.<motor>_fault`,
    which reports the SDN motor's status word and knows nothing about the controller hosting it.
    A boolean also cannot say WHICH fault, and "clock stalled" and "heap low" want very different
    responses. The slug is the state; the human sentence and the full active set are attributes.
    """

    _attr_name = "Fault"
    _attr_entity_category = EntityCategory.DIAGNOSTIC
    _attr_icon = "mdi:alert-circle-outline"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:fault"

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
            # Bitmask of every active code, not just the worst — a heap notice hidden behind a
            # dead clock is still worth having in history.
            "mask": health.get("fault_mask"),
        }


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
