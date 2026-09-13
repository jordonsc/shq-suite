"""UniFi Protect websocket watchdog.

HA core's `unifiprotect` integration cannot tell that its event websocket has
died: `uiprotect` waits on `receive()` with no timeout and no heartbeat, so a
stream that simply stops leaves the integration blocked for ever, reporting
every camera as healthy while no detection ever fires again. This component
watches the socket itself and says so. See probe.py and ledger shq-suite-0054.
"""

import logging

import voluptuous as vol

from homeassistant.const import CONF_SCAN_INTERVAL, Platform
from homeassistant.core import HomeAssistant
from homeassistant.helpers import config_validation as cv, discovery
from homeassistant.helpers.typing import ConfigType

from .const import (
    CONF_NVR_HOST,
    CONF_NVR_PORT,
    CONF_STALE_AFTER,
    DEFAULT_NVR_PORT,
    DEFAULT_SCAN_INTERVAL,
    DEFAULT_STALE_AFTER,
    DOMAIN,
)
from .coordinator import ProtectWatchdogCoordinator

_LOGGER = logging.getLogger(__name__)

PLATFORMS = [Platform.BINARY_SENSOR, Platform.SENSOR]

CONFIG_SCHEMA = vol.Schema(
    {
        DOMAIN: vol.Schema(
            {
                vol.Required(CONF_NVR_HOST): cv.string,
                vol.Optional(CONF_NVR_PORT, default=DEFAULT_NVR_PORT): cv.port,
                vol.Optional(
                    CONF_SCAN_INTERVAL, default=DEFAULT_SCAN_INTERVAL
                ): cv.positive_int,
                vol.Optional(
                    CONF_STALE_AFTER, default=DEFAULT_STALE_AFTER
                ): cv.positive_int,
            }
        )
    },
    extra=vol.ALLOW_EXTRA,
)


async def async_setup(hass: HomeAssistant, config: ConfigType) -> bool:
    """Set up the watchdog from configuration.yaml."""
    if DOMAIN not in config:
        return True

    conf = config[DOMAIN]
    coordinator = ProtectWatchdogCoordinator(
        hass,
        conf[CONF_NVR_HOST],
        conf[CONF_NVR_PORT],
        conf[CONF_SCAN_INTERVAL],
        conf[CONF_STALE_AFTER],
    )
    # Not async_config_entry_first_refresh(): this is a YAML component with no
    # config entry, and that helper rejects such a coordinator outright.
    await coordinator.async_refresh()
    hass.data[DOMAIN] = coordinator

    for platform in PLATFORMS:
        hass.async_create_task(
            discovery.async_load_platform(hass, platform, DOMAIN, {}, config)
        )
    return True
