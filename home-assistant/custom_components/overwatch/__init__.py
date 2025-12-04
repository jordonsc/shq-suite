"""Overwatch Voice Control integration for Home Assistant."""
import logging
import voluptuous as vol
from homeassistant.core import HomeAssistant, ServiceCall
from homeassistant.helpers import config_validation as cv

from .const import (
    DOMAIN,
    SERVICE_SET_ALARM,
    SERVICE_VERBALISE,
    CONF_HOST,
    CONF_PORT,
    DEFAULT_PORT,
)

_LOGGER = logging.getLogger(__name__)

# Configuration schema
CONFIG_SCHEMA = vol.Schema({
    DOMAIN: vol.Schema({
        vol.Required(CONF_HOST): cv.string,
        vol.Optional(CONF_PORT, default=DEFAULT_PORT): cv.port,
    })
}, extra=vol.ALLOW_EXTRA)

# Service call schemas
SET_ALARM_SCHEMA = vol.Schema({
    vol.Required("alarm_id"): cv.string,
    vol.Required("enabled"): cv.boolean,
    vol.Optional("volume"): vol.All(vol.Coerce(float), vol.Range(min=0.0, max=2.0)),
})

VERBALISE_SCHEMA = vol.Schema({
    vol.Required("text"): cv.string,
    vol.Optional("notification_tone_id"): cv.string,
    vol.Optional("voice_id"): cv.string,
    vol.Optional("volume"): vol.All(vol.Coerce(float), vol.Range(min=0.0, max=2.0)),
})


async def async_setup(hass: HomeAssistant, config: dict):
    """Set up the Overwatch component from configuration.yaml."""
    if DOMAIN not in config:
        _LOGGER.info("Overwatch not configured in configuration.yaml")
        return True

    conf = config[DOMAIN]
    host = conf.get(CONF_HOST)
    port = conf.get(CONF_PORT, DEFAULT_PORT)

    if not host:
        _LOGGER.error("No host specified for Overwatch voice server")
        return False

    # Lazy import to avoid blocking the event loop
    from .client import OverwatchClient

    # Create client
    client = OverwatchClient(host, port)

    # Test connection
    if not await hass.async_add_executor_job(client.connect):
        _LOGGER.error(f"Failed to connect to voice server at {host}:{port}")
        return False

    _LOGGER.info(f"Connected to Overwatch voice server at {host}:{port}")

    # Store client in hass.data
    hass.data[DOMAIN] = {"client": client}

    async def handle_set_alarm(call: ServiceCall):
        """Handle the set_alarm service call."""
        alarm_id = call.data["alarm_id"]
        enabled = call.data["enabled"]
        volume = call.data.get("volume")

        _LOGGER.debug(
            f"Service call: set_alarm(alarm_id={alarm_id}, enabled={enabled}, volume={volume})"
        )

        try:
            success, message = await hass.async_add_executor_job(
                client.set_alarm, alarm_id, enabled, volume
            )

            if success:
                _LOGGER.info(f"Alarm '{alarm_id}' {'started' if enabled else 'stopped'}")
            else:
                _LOGGER.error(f"Failed to set alarm: {message}")

        except Exception as e:
            _LOGGER.error(f"Error calling set_alarm service: {e}")

    async def handle_verbalise(call: ServiceCall):
        """Handle the verbalise service call."""
        text = call.data["text"]
        notification_tone_id = call.data.get("notification_tone_id")
        voice_id = call.data.get("voice_id")
        volume = call.data.get("volume")

        _LOGGER.debug(
            f"Service call: verbalise(text='{text}', tone={notification_tone_id}, "
            f"voice={voice_id}, volume={volume})"
        )

        try:
            success, message = await hass.async_add_executor_job(
                client.verbalise, text, notification_tone_id, voice_id, volume
            )

            if success:
                _LOGGER.info(f"Successfully verbalised text: '{text[:50]}...'")
            else:
                _LOGGER.error(f"Failed to verbalise: {message}")

        except Exception as e:
            _LOGGER.error(f"Error calling verbalise service: {e}")

    # Register services
    hass.services.async_register(
        DOMAIN,
        SERVICE_SET_ALARM,
        handle_set_alarm,
        schema=SET_ALARM_SCHEMA,
    )

    hass.services.async_register(
        DOMAIN,
        SERVICE_VERBALISE,
        handle_verbalise,
        schema=VERBALISE_SCHEMA,
    )

    _LOGGER.info("Overwatch services registered: set_alarm, verbalise")

    return True


async def async_unload_entry(hass: HomeAssistant, entry):
    """Unload a config entry."""
    if DOMAIN in hass.data:
        client = hass.data[DOMAIN].get("client")
        if client:
            await hass.async_add_executor_job(client.disconnect)
        hass.data.pop(DOMAIN)

    return True
