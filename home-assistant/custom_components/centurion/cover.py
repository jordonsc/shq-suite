import asyncio
import logging
import requests
from datetime import timedelta
from homeassistant.components.cover import CoverEntity, CoverEntityFeature
from homeassistant.const import STATE_CLOSED, STATE_OPEN, STATE_OPENING, STATE_CLOSING
from homeassistant.exceptions import HomeAssistantError
from .const import DOMAIN, CONF_IP_ADDRESS, CONF_API_KEY

_LOGGER = logging.getLogger(__name__)

SCAN_INTERVAL = timedelta(seconds=30)

async def async_setup_entry(hass, config_entry, async_add_entities):
    ip = config_entry.data[CONF_IP_ADDRESS]
    api_key = config_entry.data[CONF_API_KEY]
    async_add_entities([CenturionGarageDoor(ip, api_key)], update_before_add=True)

class CenturionGarageDoor(CoverEntity):
    def __init__(self, ip, api_key):
        self._ip = ip
        self._api_key = api_key
        self._state = STATE_CLOSED
        self._position = 0
        self._available = True
        self._attr_unique_id = f"centurion_garage_{ip.replace('.', '_')}"

    def _base_url(self):
        return f"http://{self._ip}/api?key={self._api_key}"

    def _api_call(self, params):
        return requests.get(f"{self._base_url()}&{params}", timeout=5)

    @property
    def available(self):
        return self._available

    @property
    def device_info(self):
        return {
            "identifiers": {(DOMAIN, self._ip)},
            "name": "Centurion Garage Door",
            "manufacturer": "Centurion",
            "model": "Smart Garage"
        }

    @property
    def device_class(self):
        return "garage"

    @property
    def supported_features(self):
        return (
            CoverEntityFeature.OPEN
            | CoverEntityFeature.CLOSE
            | CoverEntityFeature.STOP
            | CoverEntityFeature.SET_POSITION
        )

    @property
    def name(self):
        return "Centurion Garage Door"

    @property
    def is_closed(self):
        return self._state == STATE_CLOSED

    @property
    def is_opening(self):
        return self._state == STATE_OPENING

    @property
    def is_closing(self):
        return self._state == STATE_CLOSING

    @property
    def current_cover_position(self):
        return self._position

    async def async_update(self):
        try:
            response = await self.hass.async_add_executor_job(
                self._api_call, "status=json"
            )
            data = response.json()
            door_state = str(data.get("door", "")).lower()

            if not self._available:
                _LOGGER.info("Centurion controller back online, door state: %s", door_state)
            self._available = True

            old_state = self._state
            if door_state.startswith("opening"):
                self._state = STATE_OPENING
                self._position = 50
            elif door_state.startswith("closing"):
                self._state = STATE_CLOSING
                self._position = 50
            elif door_state.startswith("close") or door_state.startswith("closed"):
                self._state = STATE_CLOSED
                self._position = 0
            elif door_state.startswith("open"):
                self._state = STATE_OPEN
                self._position = 100
            elif "stopped" in door_state or "error" in door_state:
                self._state = STATE_OPEN
                self._position = 50
                _LOGGER.warning("Door in stopped/error state: %s", door_state)
            else:
                _LOGGER.warning("Unexpected door state: %s", door_state)
                self._state = STATE_OPEN
                self._position = 50

            if old_state != self._state:
                _LOGGER.info(
                    "Centurion door state changed: %s -> %s (raw: %s)",
                    old_state, self._state, door_state
                )

        except Exception as e:
            if self._available:
                _LOGGER.error("Centurion controller unreachable: %s", e)
            self._available = False

    def _get_door_state(self):
        """Poll the controller and return the raw door state string."""
        response = self._api_call("status=json")
        data = response.json()
        return str(data.get("door", "")).lower()

    async def _send_command_with_retry(self, command, expected_states):
        """Send a door command and hammer-poll until the door state changes.

        Resends the command every 200ms if the door hasn't moved.
        Raises HomeAssistantError after 3 seconds.
        """
        initial_state = await self.hass.async_add_executor_job(self._get_door_state)
        _LOGGER.warning("Centurion %s command: initial door state: %s", command, initial_state)

        # If already in the expected state, nothing to do.
        # Use startswith to avoid false matches like "open" matching "opener reset".
        if any(initial_state.startswith(s) for s in expected_states):
            _LOGGER.warning("Centurion %s command: door already in expected state (%s)", command, initial_state)
            return initial_state

        attempts = 0
        elapsed = 0.0
        timeout = 3.0
        poll_interval = 0.2

        while elapsed < timeout:
            response = await self.hass.async_add_executor_job(self._api_call, f"door={command}")
            attempts += 1
            _LOGGER.warning(
                "Centurion %s command: attempt %d, HTTP %s, body: %s",
                command, attempts, response.status_code, response.text,
            )

            await asyncio.sleep(poll_interval)
            elapsed += poll_interval

            door_state = await self.hass.async_add_executor_job(self._get_door_state)
            if any(door_state.startswith(s) for s in expected_states):
                _LOGGER.warning(
                    "Centurion %s command: confirmed after %d attempt(s) (%.1fs), state: %s",
                    command, attempts, elapsed, door_state,
                )
                return door_state

        raise HomeAssistantError(
            f"Centurion {command} command failed: door still '{initial_state}' after "
            f"{attempts} attempt(s) over {timeout}s"
        )

    async def async_open_cover(self, **kwargs):
        door_state = await self._send_command_with_retry("open", ["opening", "open"])
        self._state = STATE_OPENING if "opening" in door_state else STATE_OPEN
        self._position = 50 if "opening" in door_state else 100
        self.async_write_ha_state()

    async def async_close_cover(self, **kwargs):
        door_state = await self._send_command_with_retry("close", ["closing", "close"])
        self._state = STATE_CLOSING if "closing" in door_state else STATE_CLOSED
        self._position = 50 if "closing" in door_state else 0
        self.async_write_ha_state()

    async def async_stop_cover(self, **kwargs):
        try:
            response = await self.hass.async_add_executor_job(self._api_call, "door=stop")
            _LOGGER.warning("Centurion stop command: HTTP %s, body: %s", response.status_code, response.text)
        except Exception as e:
            _LOGGER.error("Error sending stop command: %s", e)

    async def async_set_cover_position(self, **kwargs):
        self.async_write_ha_state()
