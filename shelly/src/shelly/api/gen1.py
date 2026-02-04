"""Gen1 Shelly API client (HTTP REST)."""

from typing import Optional

import httpx

from shelly.api.base import ShellyAPIClient
from shelly.device import DeviceGeneration, DeviceInfo, InputMode


class Gen1Client(ShellyAPIClient):
    """Client for Gen1 Shelly devices using HTTP REST API."""

    def __init__(self, ip_address: str, device_id: str, http: httpx.AsyncClient):
        super().__init__(ip_address, device_id, http)

    async def get_device_info(self, shelly_data: Optional[dict] = None) -> DeviceInfo:
        if shelly_data is None:
            resp = await self._http.get(f"{self.base_url}/shelly")
            shelly_data = resp.json()

        auth_enabled = shelly_data.get("auth", False)
        device_type = shelly_data.get("type", "")
        is_dimmer = "SHDM" in device_type

        info = DeviceInfo(
            device_id=self.device_id,
            ip_address=self.ip_address,
            generation=DeviceGeneration.GEN1,
            model=device_type,
            auth_enabled=auth_enabled,
            is_dimmer=is_dimmer,
            bluetooth_enabled=None,
        )

        if auth_enabled:
            return info

        # Fetch settings for cloud, AP, input mode, name
        settings_resp = await self._http.get(f"{self.base_url}/settings")
        settings = settings_resp.json()

        hostname = settings.get("device", {}).get("hostname", "")
        if hostname:
            info.device_id = hostname
        info.name = settings.get("name", "") or hostname
        info.firmware_version = settings.get("fw", "")
        info.cloud_enabled = settings.get("cloud", {}).get("enabled", None)
        info.wifi_ap_enabled = settings.get("ap_roaming", {}).get("enabled", None)

        # Check WiFi AP from the ap_roaming or wifi_ap settings
        ap_config = settings.get("wifi_ap", {})
        if ap_config:
            info.wifi_ap_enabled = ap_config.get("enabled", None)

        # Input modes from output components
        info.input_modes = self._parse_input_modes(settings)

        # Fetch status for update availability and calibration
        status_resp = await self._http.get(f"{self.base_url}/status")
        status = status_resp.json()

        update_info = status.get("update", {})
        info.update_available = update_info.get("has_update", None)

        if is_dimmer:
            lights_settings = settings.get("lights", [])
            if lights_settings:
                transition_ms = lights_settings[0].get("transition")
                if transition_ms is not None:
                    info.transition_time = transition_ms / 1000.0

            lights = status.get("lights", [])
            if lights:
                info.needs_calibration = lights[0].get("calibration_needed", None)

        return info

    def _parse_input_modes(self, settings: dict) -> dict[int, InputMode]:
        mode_map = {
            "toggle": InputMode.TOGGLE,
            "edge": InputMode.EDGE,
            "detached": InputMode.DETACHED,
            "momentary": InputMode.BUTTON,
            "action": InputMode.BUTTON,
        }
        modes = {}
        # Gen1 outputs are in relays[] and/or lights[] arrays
        for outputs in (settings.get("relays", []), settings.get("lights", [])):
            for i, output in enumerate(outputs):
                if i in modes:
                    continue
                btn_type = output.get("btn_type")
                if btn_type is not None:
                    modes[i] = mode_map.get(btn_type, InputMode.UNKNOWN)
        return modes

    async def disable_cloud(self) -> bool:
        resp = await self._http.post(f"{self.base_url}/settings/cloud", data={"enabled": "0"})
        return resp.status_code == 200

    async def disable_wifi_ap(self) -> bool:
        resp = await self._http.post(f"{self.base_url}/settings/ap", data={"enabled": "0"})
        return resp.status_code == 200

    async def disable_bluetooth(self) -> bool:
        # Gen1 devices have no Bluetooth
        return True

    async def set_transition_time(self, seconds: float) -> bool:
        ms = int(seconds * 1000)
        resp = await self._http.post(
            f"{self.base_url}/settings/light/0", data={"transition": str(ms)}
        )
        return resp.status_code == 200

    async def trigger_update(self) -> bool:
        resp = await self._http.get(f"{self.base_url}/ota?update=true")
        return resp.status_code == 200

    async def calibrate(self) -> bool:
        resp = await self._http.get(f"{self.base_url}/light/0?calibrate=true")
        return resp.status_code == 200
