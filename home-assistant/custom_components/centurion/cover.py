import logging
import requests
from homeassistant.components.cover import CoverEntity, CoverEntityFeature
from homeassistant.const import STATE_CLOSED, STATE_OPEN, STATE_OPENING, STATE_CLOSING
from .const import DOMAIN, CONF_IP_ADDRESS, CONF_API_KEY

_LOGGER = logging.getLogger(__name__)

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
        self._attr_unique_id = f"centurion_garage_{ip.replace('.', '_')}"

    def _base_url(self):
        return f"http://{self._ip}/api?key={self._api_key}"

    def _api_call(self, params):
        return requests.get(f"{self._base_url()}&{params}", timeout=5)

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
            _LOGGER.debug("Centurion returned door state: %s", door_state)

            if "opening" in door_state:
                self._state = STATE_OPENING
                self._position = 50
            elif "closing" in door_state:
                self._state = STATE_CLOSING
                self._position = 50
            elif "open" in door_state:
                self._state = STATE_OPEN
                self._position = 100
            elif "close" in door_state:
                self._state = STATE_CLOSED
                self._position = 0
            elif "stopped" in door_state or "error" in door_state:
                self._state = STATE_OPEN
                self._position = 50
                _LOGGER.warning("Door in stopped/error state: %s", door_state)
            else:
                _LOGGER.warning("Unexpected door state: %s", door_state)
                self._state = STATE_OPEN
                self._position = 50

        except Exception as e:
            _LOGGER.error("Error updating Centurion door status: %s", e)

    async def async_open_cover(self, **kwargs):
        try:
            await self.hass.async_add_executor_job(self._api_call, "door=open")
            self._state = STATE_OPENING
            self._position = 50
            self.async_write_ha_state()
        except Exception as e:
            _LOGGER.error("Error sending open command: %s", e)

    async def async_close_cover(self, **kwargs):
        try:
            await self.hass.async_add_executor_job(self._api_call, "door=close")
            self._state = STATE_CLOSING
            self._position = 50
            self.async_write_ha_state()
        except Exception as e:
            _LOGGER.error("Error sending close command: %s", e)

    async def async_stop_cover(self, **kwargs):
        try:
            await self.hass.async_add_executor_job(self._api_call, "door=stop")
        except Exception as e:
            _LOGGER.error("Error sending stop command: %s", e)

    async def async_set_cover_position(self, **kwargs):
        self.async_write_ha_state()
