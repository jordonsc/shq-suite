"""Websocket consumer extracting hub DPS input states from UniFi Access device updates.

Workaround for a UniFi firmware bug: the UA hub's wire-presence detection can
false-negative on the DPS terminals (wiring_state_d1-dps-pos/neg = off), which makes
the Access controller publish door_position_status = "none" on the developer API even
though the raw DPS input works and is streamed to the controller. The raw input value
(e.g. input_d1_dps) still arrives in access.data.device.update websocket messages —
this consumer reads it from there. Retire once Ubiquiti fix wiring detection.

Also tracks the maglock relay state (which the controller publishes correctly but the
core integration exposes no entity for) and offers a lock_now action to re-engage a
held-open unlock (e.g. REX pressed but the door never cycled).
"""

import logging
from datetime import timedelta
from typing import Any

from unifi_access_api import (
    DoorLockRelayStatus,
    DoorLockRule,
    DoorLockRuleType,
    UnifiAccessApiClient,
)

from homeassistant.core import HomeAssistant
from homeassistant.helpers.aiohttp_client import async_get_clientsession
from homeassistant.helpers.dispatcher import async_dispatcher_send
from homeassistant.helpers.event import async_track_time_interval
from homeassistant.util import dt as dt_util

from .const import (
    CONF_DEVICE_ID,
    CONF_DOOR_ID,
    CONF_INPUT_KEY,
    DEFAULT_INPUT_KEY,
    EVENT_DEVICE_UPDATE,
    EVENT_LOCATION_UPDATE_LEGACY,
    EVENT_LOCATION_UPDATE_V2,
    EVENT_V2_DEVICE_UPDATE,
    LOCK_POLL_SECONDS,
    SIGNAL_UPDATE,
)

_LOGGER = logging.getLogger(__name__)


def _wiring_keys(input_key: str) -> tuple[str, str] | None:
    """Derive the wire-detection config keys from an input key.

    input_d1_dps -> (wiring_state_d1-dps-pos, wiring_state_d1-dps-neg)
    """
    parts = input_key.split("_")
    if len(parts) != 3 or parts[0] != "input":
        return None
    port, name = parts[1], parts[2]
    return (f"wiring_state_{port}-{name}-pos", f"wiring_state_{port}-{name}-neg")


class UnifiAccessDpsHub:
    """Holds the websocket connection and the latest state per watched door."""

    def __init__(
        self,
        hass: HomeAssistant,
        host: str,
        api_token: str,
        verify_ssl: bool,
        doors: list[dict[str, Any]],
    ) -> None:
        self._hass = hass
        self._host = host
        self._api_token = api_token
        self._verify_ssl = verify_ssl
        self._client: UnifiAccessApiClient | None = None
        self._websocket = None
        self._unsub_poll = None
        self.doors: dict[str, dict[str, Any]] = {d[CONF_DEVICE_ID]: d for d in doors}
        self._door_to_device: dict[str, str] = {
            d[CONF_DOOR_ID]: d[CONF_DEVICE_ID] for d in doors if d.get(CONF_DOOR_ID)
        }
        self.states: dict[str, dict[str, Any]] = {
            device_id: {
                "value": None,
                "updated_at": None,
                "wiring_detected": None,
                "lock": None,
                "lock_updated_at": None,
            }
            for device_id in self.doors
        }
        self.connected = False

    async def async_start(self) -> None:
        """Start the websocket connection (reconnects internally with backoff)."""
        session = async_get_clientsession(self._hass, verify_ssl=self._verify_ssl)

        def _make_client() -> UnifiAccessApiClient:
            # Client construction builds an SSLContext (blocking I/O) — keep it
            # off the event loop.
            return UnifiAccessApiClient(
                self._host, self._api_token, session, verify_ssl=self._verify_ssl
            )

        self._client = await self._hass.async_add_executor_job(_make_client)
        self._websocket = self._client.start_websocket(
            {},
            on_connect=self._on_connect,
            on_disconnect=self._on_disconnect,
            on_raw_message=self._on_raw_message,
        )
        if self._door_to_device:
            # Seed lock state now, then keep it fresh as a fallback for missed
            # websocket pushes.
            await self._async_poll_lock()
            self._unsub_poll = async_track_time_interval(
                self._hass,
                self._async_poll_lock,
                timedelta(seconds=LOCK_POLL_SECONDS),
            )

    async def async_stop(self) -> None:
        """Stop the websocket connection and the lock poll."""
        if self._unsub_poll is not None:
            self._unsub_poll()
            self._unsub_poll = None
        if self._websocket is not None:
            await self._websocket.stop()
            self._websocket = None

    async def async_lock_now(self, device_id: str) -> None:
        """End any held unlock and re-engage the lock immediately."""
        door = self.doors.get(device_id)
        if door is None or not door.get(CONF_DOOR_ID):
            raise ValueError(f"No door with a door_id configured for {device_id}")
        await self._client.set_door_lock_rule(
            door[CONF_DOOR_ID], DoorLockRule(type=DoorLockRuleType.LOCK_NOW)
        )
        _LOGGER.info("lock_now sent for door %s", door[CONF_DOOR_ID])
        await self._async_poll_lock()

    async def _async_poll_lock(self, _now=None) -> None:
        """Refresh lock relay state from the doors endpoint."""
        if self._client is None:
            return
        try:
            doors = await self._client.get_doors()
        except Exception as err:  # noqa: BLE001 — poll is best-effort
            _LOGGER.debug("Lock poll failed: %s", err)
            return
        for door in doors:
            device_id = self._door_to_device.get(door.id)
            if device_id is None:
                continue
            lock = (
                "locked"
                if door.door_lock_relay_status == DoorLockRelayStatus.LOCK
                else "unlocked"
            )
            self._set_lock(device_id, lock)

    def _set_lock(self, device_id: str, lock: str) -> None:
        state = self.states[device_id]
        if state["lock"] == lock:
            return
        state["lock"] = lock
        state["lock_updated_at"] = dt_util.utcnow().isoformat()
        _LOGGER.debug("Lock state for %s = %s", device_id, lock)
        async_dispatcher_send(self._hass, SIGNAL_UPDATE)

    def _on_connect(self) -> None:
        self.connected = True
        _LOGGER.info("Connected to UniFi Access websocket")
        async_dispatcher_send(self._hass, SIGNAL_UPDATE)

    def _on_disconnect(self) -> None:
        self.connected = False
        _LOGGER.warning("Disconnected from UniFi Access websocket")
        async_dispatcher_send(self._hass, SIGNAL_UPDATE)

    def _on_raw_message(self, raw: dict[str, Any]) -> None:
        event = raw.get("event")
        data = raw.get("data") or {}
        if event == EVENT_DEVICE_UPDATE:
            self._handle_device_configs(data)
        elif event in (EVENT_LOCATION_UPDATE_V2, EVENT_LOCATION_UPDATE_LEGACY):
            self._handle_location_state(data.get("id"), data.get("state") or {})
        elif event == EVENT_V2_DEVICE_UPDATE:
            for loc in data.get("location_states") or []:
                if isinstance(loc, dict):
                    self._handle_location_state(loc.get("location_id"), loc)

    def _handle_location_state(self, door_id: str | None, state: dict[str, Any]) -> None:
        """Take the lock relay state from a door-keyed websocket update."""
        device_id = self._door_to_device.get(door_id)
        if device_id is None:
            return
        lock = state.get("lock")
        if lock in ("locked", "unlocked"):
            self._set_lock(device_id, lock)

    def _handle_device_configs(self, data: dict[str, Any]) -> None:
        """Take the raw DPS input from a hub device.update's configs."""
        device_id = data.get("unique_id")
        if device_id not in self.doors:
            return

        configs = {
            c.get("key"): c
            for c in data.get("configs") or []
            if isinstance(c, dict) and c.get("key")
        }
        input_key = self.doors[device_id].get(CONF_INPUT_KEY, DEFAULT_INPUT_KEY)
        entry = configs.get(input_key)
        if entry is None:
            _LOGGER.debug(
                "device.update for %s carried no %s config", device_id, input_key
            )
            return

        state = self.states[device_id]
        state["value"] = entry.get("value")
        state["updated_at"] = entry.get("update_time")

        if wiring := _wiring_keys(input_key):
            pos, neg = (configs.get(k, {}).get("value") for k in wiring)
            if pos is not None or neg is not None:
                state["wiring_detected"] = pos == "on" or neg == "on"

        _LOGGER.debug(
            "DPS input %s on %s = %s", input_key, device_id, state["value"]
        )
        async_dispatcher_send(self._hass, SIGNAL_UPDATE)
