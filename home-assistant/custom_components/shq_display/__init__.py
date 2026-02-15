"""SHQ Display integration for Home Assistant."""
import logging
import voluptuous as vol
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.const import Platform, EVENT_HOMEASSISTANT_STOP
from homeassistant.helpers import config_validation as cv, discovery

from .const import DOMAIN
from .coordinator import SHQDisplayCoordinator

_LOGGER = logging.getLogger(__name__)

PLATFORMS = [Platform.LIGHT, Platform.NUMBER, Platform.SENSOR]


async def async_setup(hass: HomeAssistant, config: dict):
    """Set up the SHQ Display component from configuration.yaml."""
    hass.data.setdefault(DOMAIN, {})

    if DOMAIN not in config:
        return True

    # Create coordinators for each device
    coordinators = {}

    for device_id, device_config in config[DOMAIN].items():
        host = device_config.get("host")
        port = device_config.get("port", 8765)
        name = device_config.get("name", f"SHQ Display {device_id}")

        if not host:
            _LOGGER.error(f"No host specified for device {device_id}")
            continue

        coordinator = SHQDisplayCoordinator(hass, device_id, name, host, port)
        await coordinator.async_start()
        coordinators[device_id] = coordinator
        _LOGGER.info(f"Coordinator created for {name}")

    hass.data[DOMAIN] = coordinators

    # Register navigate service
    async def handle_navigate(call):
        device_id = call.data["device_id"]
        url = call.data["url"]
        coordinator = hass.data[DOMAIN].get(device_id)
        if coordinator is None:
            _LOGGER.error(f"Unknown device_id: {device_id}")
            return
        await coordinator.async_send_command(coordinator.client.navigate, url)

    hass.services.async_register(
        DOMAIN,
        "navigate",
        handle_navigate,
        schema=vol.Schema({
            vol.Required("device_id"): cv.string,
            vol.Required("url"): cv.url,
        }),
    )

    # Register shutdown handler
    async def async_shutdown(event):
        """Shutdown coordinators on Home Assistant stop."""
        for coordinator in coordinators.values():
            await coordinator.async_shutdown()

    hass.bus.async_listen_once(EVENT_HOMEASSISTANT_STOP, async_shutdown)

    # Forward setup to platforms
    await discovery.async_load_platform(
        hass, Platform.LIGHT, DOMAIN, {}, config
    )
    await discovery.async_load_platform(
        hass, Platform.NUMBER, DOMAIN, {}, config
    )
    await discovery.async_load_platform(
        hass, Platform.SENSOR, DOMAIN, {}, config
    )

    return True


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry):
    """Set up SHQ Display from a config entry (not used, YAML only)."""
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry):
    """Unload a config entry (not used, YAML only)."""
    return True
