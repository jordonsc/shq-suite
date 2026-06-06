"""Config flow for Somfy SDN.

Two entry paths:
- Manual (`async_step_user`): host + port form, for when discovery isn't available.
- Zeroconf (`async_step_zeroconf`): the firmware advertises `_somfy-sdn._tcp` with a TXT `id`
  = the ESP32 MAC. We key the config entry on that MAC, so on every re-announcement (e.g. after
  a reboot onto a new DHCP lease) we rewrite the stored host to the current IP — the device is
  self-healing and never needs a manual IP or a router reservation.
"""

from typing import Any

import voluptuous as vol

from homeassistant import config_entries

try:  # HA 2024.4+ moved the dataclass here; fall back for older cores.
    from homeassistant.helpers.service_info.zeroconf import ZeroconfServiceInfo
except ImportError:  # pragma: no cover
    from homeassistant.components.zeroconf import ZeroconfServiceInfo

from .const import CONF_HOST, CONF_PORT, DEFAULT_PORT, DOMAIN


class SomfySdnConfigFlow(config_entries.ConfigFlow, domain=DOMAIN):
    """Manual + zeroconf config flow."""

    VERSION = 2  # v2: entity unique_ids / device identifiers keyed on MAC (was host:port)

    def __init__(self) -> None:
        self._host: str | None = None
        self._port: int = DEFAULT_PORT
        self._name: str | None = None

    # ---- manual ----------------------------------------------------------

    async def async_step_user(self, user_input: dict[str, Any] | None = None):
        if user_input is not None:
            await self.async_set_unique_id(
                f"{user_input[CONF_HOST]}:{user_input[CONF_PORT]}"
            )
            self._abort_if_unique_id_configured()
            return self.async_create_entry(
                title=f"Somfy SDN ({user_input[CONF_HOST]})",
                data=user_input,
            )

        return self.async_show_form(
            step_id="user",
            data_schema=vol.Schema(
                {
                    vol.Required(CONF_HOST): str,
                    vol.Required(CONF_PORT, default=DEFAULT_PORT): int,
                }
            ),
        )

    # ---- zeroconf --------------------------------------------------------

    async def async_step_zeroconf(self, discovery_info: ZeroconfServiceInfo):
        # Host/IP across HA versions (newer expose ip_address, older host).
        host = getattr(discovery_info, "host", None)
        if host is None and getattr(discovery_info, "ip_address", None) is not None:
            host = str(discovery_info.ip_address)
        port = discovery_info.port or DEFAULT_PORT
        props = discovery_info.properties or {}

        # Stable identity: TXT `id` (MAC). Fall back to the mDNS hostname if absent.
        hostname = (discovery_info.hostname or "").removesuffix(".local.")
        unique_id = props.get("id") or hostname
        if not unique_id or host is None:
            return self.async_abort(reason="cannot_connect")

        await self.async_set_unique_id(unique_id)
        # If this device already has an entry, just refresh its host/port to wherever it is now
        # and stop (self-healing across IP changes). Otherwise continue to a confirm step.
        self._abort_if_unique_id_configured(updates={CONF_HOST: host, CONF_PORT: port})

        self._host = host
        self._port = port
        self._name = hostname or host
        # Shown in the discovered-devices card.
        self.context["title_placeholders"] = {"name": self._name}
        return await self.async_step_zeroconf_confirm()

    async def async_step_zeroconf_confirm(self, user_input: dict[str, Any] | None = None):
        if user_input is not None:
            return self.async_create_entry(
                title=f"Somfy SDN ({self._name})",
                data={CONF_HOST: self._host, CONF_PORT: self._port},
            )
        return self.async_show_form(
            step_id="zeroconf_confirm",
            description_placeholders={"name": self._name or "", "host": self._host or ""},
        )
