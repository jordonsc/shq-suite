"""DOSA integration for Home Assistant."""
import logging
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.const import Platform, EVENT_HOMEASSISTANT_STOP
from homeassistant.helpers import discovery

from .const import DOMAIN
from .coordinator import DosaCoordinator

_LOGGER = logging.getLogger(__name__)

PLATFORMS = [Platform.COVER, Platform.BUTTON]


async def async_setup(hass: HomeAssistant, config: dict):
    """Set up the DOSA component from configuration.yaml."""
    hass.data.setdefault(DOMAIN, {})

    if DOMAIN not in config:
        return True

    # Create coordinators for each device
    coordinators = {}

    for device_id, device_config in config[DOMAIN].items():
        host = device_config.get("host")
        port = device_config.get("port", 8766)
        name = device_config.get("name", f"DOSA {device_id}")

        if not host:
            _LOGGER.error(f"No host specified for device {device_id}")
            continue

        coordinator = DosaCoordinator(hass, device_id, name, host, port)
        await coordinator.async_start()
        coordinators[device_id] = coordinator
        _LOGGER.info(f"Coordinator created for {name}")

    hass.data[DOMAIN] = coordinators

    # Register shutdown handler
    async def async_shutdown(event):
        """Shutdown coordinators on Home Assistant stop."""
        for coordinator in coordinators.values():
            await coordinator.async_shutdown()

    hass.bus.async_listen_once(EVENT_HOMEASSISTANT_STOP, async_shutdown)

    # Forward setup to platforms
    await discovery.async_load_platform(
        hass, Platform.COVER, DOMAIN, {}, config
    )
    await discovery.async_load_platform(
        hass, Platform.BUTTON, DOMAIN, {}, config
    )

    return True


async def async_setup_entry(hass: HomeAssistant, entry: ConfigEntry):
    """Set up DOSA from a config entry (not used, YAML only)."""
    return True


async def async_unload_entry(hass: HomeAssistant, entry: ConfigEntry):
    """Unload a config entry (not used, YAML only)."""
    return True
