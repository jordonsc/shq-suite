"""Config flow for Actron MITM Controller. Single-step IP+port form."""

from typing import Any

import voluptuous as vol

from homeassistant import config_entries

from .const import CONF_HOST, CONF_PORT, DEFAULT_PORT, DOMAIN


class ActronMitmConfigFlow(config_entries.ConfigFlow, domain=DOMAIN):
    """Config flow that just asks for the ESP32's IP address (and an optional port)."""

    VERSION = 1

    async def async_step_user(self, user_input: dict[str, Any] | None = None):
        if user_input is not None:
            await self.async_set_unique_id(
                f"{user_input[CONF_HOST]}:{user_input[CONF_PORT]}"
            )
            self._abort_if_unique_id_configured()
            return self.async_create_entry(
                title=f"Actron MITM ({user_input[CONF_HOST]})",
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
