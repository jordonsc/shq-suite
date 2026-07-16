"""UniFi Access DPS workaround integration.

Reads raw hub DPS input values from the UniFi Access developer websocket, bypassing
the controller's broken door_position_status derivation. See coordinator.py for why.
"""

import logging

import voluptuous as vol

from homeassistant.const import CONF_HOST, CONF_NAME, EVENT_HOMEASSISTANT_STOP, Platform
from homeassistant.core import HomeAssistant, ServiceCall
from homeassistant.helpers import config_validation as cv, discovery

from .const import (
    CONF_API_TOKEN,
    CONF_DEVICE_ID,
    CONF_DOOR_ID,
    CONF_DOORS,
    CONF_INPUT_KEY,
    CONF_VERIFY_SSL,
    DEFAULT_INPUT_KEY,
    DOMAIN,
    SERVICE_LOCK_NOW,
)
from .coordinator import UnifiAccessDpsHub

_LOGGER = logging.getLogger(__name__)

DOOR_SCHEMA = vol.Schema(
    {
        vol.Required(CONF_DEVICE_ID): cv.string,
        vol.Required(CONF_NAME): cv.string,
        vol.Optional(CONF_DOOR_ID): cv.string,
        vol.Optional(CONF_INPUT_KEY, default=DEFAULT_INPUT_KEY): cv.string,
    }
)

SERVICE_LOCK_NOW_SCHEMA = vol.Schema({vol.Required(CONF_DEVICE_ID): cv.string})

CONFIG_SCHEMA = vol.Schema(
    {
        DOMAIN: vol.Schema(
            {
                vol.Required(CONF_HOST): cv.string,
                vol.Required(CONF_API_TOKEN): cv.string,
                vol.Optional(CONF_VERIFY_SSL, default=False): cv.boolean,
                vol.Required(CONF_DOORS): vol.All(cv.ensure_list, [DOOR_SCHEMA]),
            }
        )
    },
    extra=vol.ALLOW_EXTRA,
)


async def async_setup(hass: HomeAssistant, config: dict) -> bool:
    """Set up the component from configuration.yaml."""
    if DOMAIN not in config:
        return True

    conf = config[DOMAIN]
    hub = UnifiAccessDpsHub(
        hass,
        conf[CONF_HOST],
        conf[CONF_API_TOKEN],
        conf[CONF_VERIFY_SSL],
        conf[CONF_DOORS],
    )
    await hub.async_start()
    hass.data[DOMAIN] = hub

    async def _shutdown(event) -> None:
        await hub.async_stop()

    hass.bus.async_listen_once(EVENT_HOMEASSISTANT_STOP, _shutdown)

    async def _handle_lock_now(call: ServiceCall) -> None:
        await hub.async_lock_now(call.data[CONF_DEVICE_ID])

    hass.services.async_register(
        DOMAIN, SERVICE_LOCK_NOW, _handle_lock_now, schema=SERVICE_LOCK_NOW_SCHEMA
    )

    hass.async_create_task(
        discovery.async_load_platform(hass, Platform.BINARY_SENSOR, DOMAIN, {}, config)
    )
    return True
