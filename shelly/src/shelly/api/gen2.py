"""Gen2 Shelly API client (JSON-RPC over HTTP)."""

from typing import Any, Optional

import httpx

from shelly.api.base import ShellyAPIClient
from shelly.device import DeviceGeneration, DeviceInfo, InputMode


class Gen2Client(ShellyAPIClient):
    """Client for Gen2 Shelly devices using JSON-RPC API."""

    def __init__(self, ip_address: str, device_id: str, http: httpx.AsyncClient):
        super().__init__(ip_address, device_id, http)
        self._rpc_id = 0

    async def _rpc_call(self, method: str, params: Optional[dict] = None) -> Any:
        self._rpc_id += 1
        payload = {
            "id": self._rpc_id,
            "method": method,
        }
        if params:
            payload["params"] = params

        resp = await self._http.post(f"{self.base_url}/rpc", json=payload)
        data = resp.json()
        if "error" in data:
            raise RuntimeError(f"RPC error: {data['error']}")
        return data.get("result")

    async def get_device_info(self, shelly_data: Optional[dict] = None) -> DeviceInfo:
        if shelly_data is None:
            resp = await self._http.get(f"{self.base_url}/shelly")
            shelly_data = resp.json()

        auth_enabled = shelly_data.get("auth_en", False)
        model = shelly_data.get("model", shelly_data.get("app", "Unknown"))
        device_id_from_shelly = shelly_data.get("id", self.device_id)
        is_dimmer = "dimmer" in model.lower() or "dim" in shelly_data.get("app", "").lower()

        info = DeviceInfo(
            device_id=device_id_from_shelly,
            ip_address=self.ip_address,
            generation=DeviceGeneration.GEN2,
            model=model,
            auth_enabled=auth_enabled,
            is_dimmer=is_dimmer,
        )

        if auth_enabled:
            return info

        # Fetch full config
        config = await self._rpc_call("Shelly.GetConfig")

        # Name
        sys_config = config.get("sys", {})
        info.name = sys_config.get("device", {}).get("name", "")
        info.firmware_version = shelly_data.get("fw_id", shelly_data.get("ver", ""))

        # Cloud
        cloud_config = config.get("cloud", {})
        info.cloud_enabled = cloud_config.get("enable", None)

        # Bluetooth
        ble_config = config.get("ble", {})
        info.bluetooth_enabled = ble_config.get("enable", None)

        # WiFi AP
        wifi_config = config.get("wifi", {})
        ap_config = wifi_config.get("ap", {})
        info.wifi_ap_enabled = ap_config.get("enable", None)

        # Input modes - read in_mode from output components (light:N, switch:N)
        info.input_modes = self._parse_input_modes(config)

        # Transition time (dimmers)
        if is_dimmer:
            light_config = config.get("light:0", {})
            transition = light_config.get("transition_duration")
            if transition is not None:
                info.transition_time = float(transition)

        # Status for update and calibration
        try:
            update_result = await self._rpc_call("Shelly.CheckForUpdate")
            stable = update_result.get("stable", {}) if update_result else {}
            info.update_available = stable.get("version") is not None
        except Exception:
            info.update_available = None

        if is_dimmer:
            try:
                status = await self._rpc_call("Shelly.GetStatus")
                light_status = status.get("light:0", {})
                info.needs_calibration = not light_status.get("calibrated", True)
            except Exception:
                info.needs_calibration = None

        return info

    def _parse_input_modes(self, config: dict) -> dict[int, InputMode]:
        mode_map = {
            "follow": InputMode.TOGGLE,
            "flip": InputMode.EDGE,
            "detached": InputMode.DETACHED,
            "momentary": InputMode.BUTTON,
            "activate": InputMode.BUTTON,
        }
        modes = {}
        for key, value in config.items():
            if not isinstance(value, dict) or "in_mode" not in value:
                continue
            # Extract index from keys like "light:0", "switch:1"
            parts = key.split(":")
            if len(parts) != 2:
                continue
            try:
                idx = int(parts[1])
            except ValueError:
                continue
            raw = value["in_mode"]
            modes[idx] = mode_map.get(raw, InputMode.UNKNOWN)
        return modes

    async def disable_cloud(self) -> bool:
        try:
            await self._rpc_call("Cloud.SetConfig", {"config": {"enable": False}})
            return True
        except Exception:
            return False

    async def disable_wifi_ap(self) -> bool:
        try:
            await self._rpc_call("WiFi.SetConfig", {"config": {"ap": {"enable": False}}})
            return True
        except Exception:
            return False

    async def disable_bluetooth(self) -> bool:
        try:
            await self._rpc_call("BLE.SetConfig", {"config": {"enable": False}})
            return True
        except Exception:
            return False

    async def set_transition_time(self, seconds: float) -> bool:
        try:
            await self._rpc_call(
                "Light.SetConfig", {"id": 0, "config": {"transition_duration": seconds}}
            )
            return True
        except Exception:
            return False

    async def trigger_update(self) -> bool:
        try:
            await self._rpc_call("Shelly.Update")
            return True
        except Exception:
            return False

    async def calibrate(self) -> bool:
        try:
            await self._rpc_call("Light.Calibrate", {"id": 0})
            return True
        except Exception:
            return False
