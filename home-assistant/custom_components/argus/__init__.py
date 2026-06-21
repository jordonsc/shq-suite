"""Argus AI alarm-assessment integration for Home Assistant."""
import logging

from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.const import Platform, EVENT_HOMEASSISTANT_STOP
from homeassistant.helpers import discovery

from .const import DOMAIN
from .coordinator import ArgusCoordinator

_LOGGER = logging.getLogger(__name__)

PLATFORMS = [Platform.BINARY_SENSOR, Platform.SENSOR, Platform.BUTTON]


async def async_setup(hass: HomeAssistant, config: dict):
    """Set up the Argus component from configuration.yaml."""
    hass.data.setdefault(DOMAIN, {})

    if DOMAIN not in config:
        return True

    conf = config[DOMAIN]
    host = conf.get("host")
    port = conf.get("port", 8770)
    name = conf.get("name", "Argus")

    if not host:
        _LOGGER.error("No host specified for Argus")
        return True

    coordinator = ArgusCoordinator(hass, name, host, port)
    await coordinator.async_start()
    hass.data[DOMAIN] = coordinator
    _LOGGER.info(f"Coordinator created for {name}")

    # Register shutdown handler
    async def async_shutdown(event):
        """Shutdown the coordinator on Home Assistant stop."""
        await coordinator.async_shutdown()

    hass.bus.async_listen_once(EVENT_HOMEASSISTANT_STOP, async_shutdown)

    # Forward setup to platforms
    for platform in PLATFORMS:
        await discovery.async_load_platform(
            hass, platform, DOMAIN, {}, config
        )

    return True


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry):
    """Set up Argus from a config entry (not used, YAML only)."""
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry):
    """Unload a config entry (not used, YAML only)."""
    return True
