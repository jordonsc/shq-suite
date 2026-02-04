"""mDNS discovery for Shelly devices on the local network."""

import asyncio
from typing import Optional

from zeroconf import ServiceBrowser, ServiceStateChange, Zeroconf

SHELLY_SERVICE_TYPE = "_shelly._tcp.local."
DEFAULT_SCAN_TIMEOUT = 5


class ShellyDiscoveryListener:
    """Listener that collects discovered Shelly devices."""

    def __init__(self):
        self.devices: list[tuple[str, str]] = []

    def on_service_state_change(
        self,
        zeroconf: Zeroconf,
        service_type: str,
        name: str,
        state_change: ServiceStateChange,
    ):
        if state_change is not ServiceStateChange.Added:
            return

        info = zeroconf.get_service_info(service_type, name)
        if info is None:
            return

        addresses = info.parsed_addresses()
        if not addresses:
            return

        ip = addresses[0]
        # Service name format: "shellyXXXX._shelly._tcp.local."
        # Extract the device ID from the service name
        device_id = name.replace(f".{service_type}", "")

        self.devices.append((device_id, ip))


def _blocking_discover(timeout: int) -> list[tuple[str, str]]:
    """Run blocking zeroconf discovery. Called via asyncio.to_thread()."""
    zc = Zeroconf()
    listener = ShellyDiscoveryListener()

    try:
        ServiceBrowser(zc, SHELLY_SERVICE_TYPE, handlers=[listener.on_service_state_change])
        # Wait for devices to be discovered
        import time
        time.sleep(timeout)
        return listener.devices
    finally:
        zc.close()


async def discover_devices(
    timeout: int = DEFAULT_SCAN_TIMEOUT,
) -> list[tuple[str, str]]:
    """
    Discover Shelly devices on the local network via mDNS.

    Returns a list of (device_id, ip_address) tuples.
    """
    return await asyncio.to_thread(_blocking_discover, timeout)
