"""Abstract base class for Shelly API clients."""

from abc import ABC, abstractmethod
from typing import Optional

import httpx

from shelly.device import DeviceInfo


class ShellyAPIClient(ABC):
    """Base class for generation-specific Shelly API clients."""

    def __init__(self, ip_address: str, device_id: str, http: httpx.AsyncClient):
        self.ip_address = ip_address
        self.device_id = device_id
        self.base_url = f"http://{ip_address}"
        self._http = http

    @abstractmethod
    async def get_device_info(self, shelly_data: Optional[dict] = None) -> DeviceInfo:
        """Query device and return populated DeviceInfo.

        If shelly_data is provided, skip the initial /shelly probe.
        """

    @abstractmethod
    async def disable_cloud(self) -> bool:
        """Disable cloud connectivity. Returns True on success."""

    @abstractmethod
    async def disable_wifi_ap(self) -> bool:
        """Disable WiFi access point. Returns True on success."""

    @abstractmethod
    async def disable_bluetooth(self) -> bool:
        """Disable Bluetooth. Returns True on success."""

    @abstractmethod
    async def trigger_update(self) -> bool:
        """Trigger firmware update. Returns True on success."""

    @abstractmethod
    async def set_transition_time(self, seconds: float) -> bool:
        """Set light transition time. Returns True on success."""

    @abstractmethod
    async def calibrate(self) -> bool:
        """Run dimmer calibration. Returns True on success."""
