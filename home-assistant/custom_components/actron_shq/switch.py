"""Switch platform for Actron SHQ integration."""

import logging
from dataclasses import dataclass
from typing import Any

from homeassistant.components.switch import SwitchEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback
from homeassistant.helpers.update_coordinator import CoordinatorEntity

from .const import DOMAIN
from .coordinator import ActronCoordinator

_LOGGER = logging.getLogger(__name__)


@dataclass
class ActronSwitchConfig:
    """Configuration for an Actron switch entity."""

    key: str
    name: str
    icon: str
    read_attr: str
    api_method: str


SWITCH_CONFIGS = [
    ActronSwitchConfig(
        key="continuous_fan",
        name="Actron Continuous Fan",
        icon="mdi:fan-clock",
        read_attr="continuous_fan_enabled",
        api_method="set_continuous_fan",
    ),
    ActronSwitchConfig(
        key="away_mode",
        name="Actron Away Mode",
        icon="mdi:home-export-outline",
        read_attr="away_mode",
        api_method="set_away_mode",
    ),
    ActronSwitchConfig(
        key="quiet_mode",
        name="Actron Quiet Mode",
        icon="mdi:volume-off",
        read_attr="quiet_mode_enabled",
        api_method="set_quiet_mode",
    ),
    ActronSwitchConfig(
        key="turbo_mode",
        name="Actron Turbo Mode",
        icon="mdi:rocket-launch",
        read_attr="turbo_enabled",
        api_method="set_turbo_mode",
    ),
]


async def async_setup_entry(
    hass: HomeAssistant,
    entry: ConfigEntry,
    async_add_entities: AddEntitiesCallback,
) -> None:
    """Set up Actron switch entities from a config entry."""
    coordinator: ActronCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities(
        ActronSwitch(coordinator, config) for config in SWITCH_CONFIGS
    )


class ActronSwitch(CoordinatorEntity, SwitchEntity):
    """Switch entity for an Actron AC feature toggle."""

    def __init__(
        self, coordinator: ActronCoordinator, config: ActronSwitchConfig
    ) -> None:
        """Initialise the switch."""
        super().__init__(coordinator)
        self._config = config
        self._optimistic_state: bool | None = None
        self._attr_unique_id = f"{DOMAIN}_{coordinator.serial}_{config.key}"
        self._attr_name = config.name
        self._attr_icon = config.icon

    @property
    def _settings(self):
        return self.coordinator.data.user_aircon_settings

    @property
    def is_on(self) -> bool:
        """Return whether the feature is enabled."""
        if self._optimistic_state is not None:
            return self._optimistic_state
        return getattr(self._settings, self._config.read_attr, False)

    def _handle_coordinator_update(self) -> None:
        """Clear optimistic state when real data arrives."""
        self._optimistic_state = None
        super()._handle_coordinator_update()

    async def _optimistic_toggle(self, enabled: bool) -> None:
        """Toggle with optimistic state update.

        No coordinator refresh on success — optimistic state stays until the
        next scheduled poll so concurrent toggles don't clear each other.
        """
        self._optimistic_state = enabled
        self.coordinator.reset_poll_timer()
        self.async_write_ha_state()

        try:
            api_method = getattr(self.coordinator.api, self._config.api_method)
            async with self.coordinator.command_lock:
                await api_method(self.coordinator.data, enabled)
            # No async_request_refresh() — see docstring above
        except Exception:
            self._optimistic_state = None
            self.async_write_ha_state()
            raise

    async def async_turn_on(self, **kwargs: Any) -> None:
        """Turn the feature on."""
        await self._optimistic_toggle(True)

    async def async_turn_off(self, **kwargs: Any) -> None:
        """Turn the feature off."""
        await self._optimistic_toggle(False)
