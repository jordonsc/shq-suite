"""Config flow for Actron SHQ — OAuth2 device-code authentication."""

import logging

import voluptuous as vol
from actron_neo_api import ActronAirAPI
from homeassistant import config_entries

from .const import DOMAIN

_LOGGER = logging.getLogger(__name__)


class ActronSHQConfigFlow(config_entries.ConfigFlow, domain=DOMAIN):
    """Handle an Actron SHQ config flow using OAuth2 device-code."""

    VERSION = 1

    def __init__(self) -> None:
        """Initialise the config flow."""
        self._api: ActronAirAPI | None = None
        self._device_code: str | None = None

    async def async_step_user(self, user_input=None):
        """Step 1: Request a device code and show it to the user."""
        await self.async_set_unique_id(DOMAIN)
        self._abort_if_unique_id_configured()

        errors = {}

        if user_input is not None:
            # User clicked submit — move to polling step
            return await self.async_step_poll()

        # Request a device code from the Actron API
        try:
            self._api = ActronAirAPI()
            response = await self._api.request_device_code()
            self._device_code = response["device_code"]
            url = response.get(
                "verification_uri_complete",
                response.get("verification_uri", "N/A"),
            )
            code = response.get("user_code", "N/A")
            self._poll_interval = response.get("interval", 5)
        except Exception:
            _LOGGER.exception("Failed to request device code")
            errors["base"] = "unknown"
            return self.async_show_form(
                step_id="user",
                data_schema=vol.Schema({}),
                errors=errors,
            )

        return self.async_show_form(
            step_id="user",
            data_schema=vol.Schema({}),
            description_placeholders={"url": url, "code": code},
            errors=errors,
        )

    async def async_step_poll(self, user_input=None):
        """Step 2: Poll for token after user has authenticated."""
        errors = {}

        try:
            await self._api.poll_for_token(
                device_code=self._device_code,
                interval=self._poll_interval,
                timeout=300,
            )
            refresh_token = self._api.refresh_token_value
            if not refresh_token:
                errors["base"] = "auth_failed"
            else:
                await self._api.close()
                return self.async_create_entry(
                    title="Actron SHQ",
                    data={"refresh_token": refresh_token},
                )
        except Exception:
            _LOGGER.exception("Device-code polling failed")
            errors["base"] = "auth_failed"

        # Auth failed — clean up and let them try again from step 1
        if self._api:
            try:
                await self._api.close()
            except Exception:
                pass
            self._api = None
            self._device_code = None

        return self.async_show_form(
            step_id="user",
            data_schema=vol.Schema({}),
            errors=errors,
        )
