"""Button entities for Somfy SDN.

Controller: Rediscover motors. Per-motor (config category, so they live on the device page, not
dashboards): Set top/bottom limit, Identify, Reset positions, Jog up/down (by the Number step).
"""

from homeassistant.components.button import ButtonEntity
from homeassistant.config_entries import ConfigEntry
from homeassistant.const import EntityCategory
from homeassistant.core import HomeAssistant
from homeassistant.helpers.entity_platform import AddEntitiesCallback

from .const import DOMAIN
from .coordinator import SomfySdnCoordinator
from .entity import SomfySdnControllerEntity, SomfySdnMotorEntity, add_motor_entities

DEFAULT_JOG_DURATION = 20


async def async_setup_entry(
    hass: HomeAssistant, entry: ConfigEntry, async_add_entities: AddEntitiesCallback
) -> None:
    coordinator: SomfySdnCoordinator = hass.data[DOMAIN][entry.entry_id]
    async_add_entities([RediscoverButton(coordinator)])
    add_motor_entities(
        coordinator,
        entry,
        async_add_entities,
        lambda addr: [
            SetTopLimitButton(coordinator, addr),
            SetBottomLimitButton(coordinator, addr),
            IdentifyButton(coordinator, addr),
            ResetButton(coordinator, addr),
            JogButton(coordinator, addr, "up"),
            JogButton(coordinator, addr, "down"),
        ],
    )


class RediscoverButton(SomfySdnControllerEntity, ButtonEntity):
    _attr_name = "Rediscover motors"
    _attr_entity_category = EntityCategory.CONFIG
    _attr_icon = "mdi:magnify-scan"

    def __init__(self, coordinator: SomfySdnCoordinator) -> None:
        super().__init__(coordinator)
        self._attr_unique_id = f"{self._cid}:rediscover"

    async def async_press(self) -> None:
        await self.coordinator.async_send_command("rediscover")


class _MotorButton(SomfySdnMotorEntity, ButtonEntity):
    _attr_entity_category = EntityCategory.CONFIG
    _key = ""

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str) -> None:
        super().__init__(coordinator, addr)
        self._attr_unique_id = f"{self._cid}:{addr}:{self._key}"


class SetTopLimitButton(_MotorButton):
    _attr_name = "Set top limit"
    _attr_icon = "mdi:arrow-collapse-up"
    _key = "set_top_limit"

    async def async_press(self) -> None:
        await self.coordinator.async_send_command("set_top_limit", addr=self._addr)


class SetBottomLimitButton(_MotorButton):
    _attr_name = "Set bottom limit"
    _attr_icon = "mdi:arrow-collapse-down"
    _key = "set_bottom_limit"

    async def async_press(self) -> None:
        await self.coordinator.async_send_command("set_bottom_limit", addr=self._addr)


class IdentifyButton(_MotorButton):
    _attr_name = "Identify"
    _attr_icon = "mdi:eye"
    _key = "identify"

    async def async_press(self) -> None:
        await self.coordinator.async_send_command("identify", addr=self._addr)


class ResetButton(_MotorButton):
    _attr_name = "Reset positions"
    _attr_icon = "mdi:backup-restore"
    _key = "reset"

    async def async_press(self) -> None:
        await self.coordinator.async_send_command("reset", addr=self._addr)


class JogButton(_MotorButton):
    _attr_icon = "mdi:cursor-move"

    def __init__(self, coordinator: SomfySdnCoordinator, addr: str, direction: str) -> None:
        self._direction = direction
        self._key = f"jog_{direction}"
        super().__init__(coordinator, addr)
        self._attr_name = f"Jog {direction}"

    async def async_press(self) -> None:
        # Timed momentary nudge (CTRL_MOVE) — works before limits are set (Set Pro style).
        duration = self.coordinator.jog_steps.get(self._addr, DEFAULT_JOG_DURATION)
        await self.coordinator.async_send_command(
            "jog", addr=self._addr, direction=self._direction, duration=duration
        )
