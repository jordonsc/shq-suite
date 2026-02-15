"""CFA Fire Ban integration for Home Assistant."""
import logging

import voluptuous as vol
from homeassistant.core import HomeAssistant
from homeassistant.const import Platform
from homeassistant.helpers import config_validation as cv, discovery

from .const import DOMAIN, VALID_DISTRICTS
from .coordinator import CfaFireBanCoordinator

_LOGGER = logging.getLogger(__name__)

PLATFORMS = [Platform.BINARY_SENSOR, Platform.SENSOR]

CONFIG_SCHEMA = vol.Schema(
    {
        DOMAIN: vol.Schema(
            {
                vol.Optional("district", default="central"): vol.In(VALID_DISTRICTS),
                vol.Optional("name", default="CFA Fire Ban"): cv.string,
            }
        )
    },
    extra=vol.ALLOW_EXTRA,
)


async def async_setup(hass: HomeAssistant, config: dict) -> bool:
    """Set up CFA Fire Ban from configuration.yaml."""
    if DOMAIN not in config:
        return True

    conf = config[DOMAIN]
    district = conf["district"]
    name = conf["name"]

    coordinator = CfaFireBanCoordinator(hass, district)
    await coordinator.async_refresh()

    hass.data[DOMAIN] = {
        "coordinator": coordinator,
        "name": name,
        "district": district,
    }

    for platform in PLATFORMS:
        await discovery.async_load_platform(hass, platform, DOMAIN, {}, config)

    return True
