"""DeviceManager - orchestrates discovery and API calls."""

import asyncio
from typing import Optional

import click
import httpx

from shelly.api.base import ShellyAPIClient
from shelly.api.gen1 import Gen1Client
from shelly.api.gen2 import Gen2Client
from shelly.device import DeviceGeneration, DeviceInfo
from shelly.discovery import discover_devices

REQUEST_TIMEOUT = 1.0
CONNECTION_LIMIT = 50


def _make_http_client() -> httpx.AsyncClient:
    """Create a shared HTTP client with connection pooling."""
    limits = httpx.Limits(
        max_connections=CONNECTION_LIMIT,
        max_keepalive_connections=CONNECTION_LIMIT,
    )
    return httpx.AsyncClient(timeout=REQUEST_TIMEOUT, limits=limits)


class DeviceManager:
    """Orchestrates Shelly device discovery and management."""

    def _make_api_client(
        self, ip_address: str, device_id: str, shelly_data: dict, http: httpx.AsyncClient
    ) -> ShellyAPIClient:
        """Return the appropriate generation client based on /shelly probe data."""
        if shelly_data.get("gen", 1) >= 2:
            return Gen2Client(ip_address, device_id, http)
        return Gen1Client(ip_address, device_id, http)

    async def _probe_and_query(
        self, device_id: str, ip_address: str, http: httpx.AsyncClient
    ) -> DeviceInfo:
        """Single /shelly probe, then full query reusing the probe data."""
        resp = await http.get(f"http://{ip_address}/shelly")
        shelly_data = resp.json()
        client = self._make_api_client(ip_address, device_id, shelly_data, http)
        return await client.get_device_info(shelly_data=shelly_data)

    async def _query_device(
        self, device_id: str, ip_address: str, http: httpx.AsyncClient, max_retries: int = 3
    ) -> DeviceInfo:
        """Query a single device for its full info, retrying on failure."""
        last_error = None
        for attempt in range(1, max_retries + 1):
            try:
                return await self._probe_and_query(device_id, ip_address, http)
            except Exception as e:
                last_error = e
                if attempt < max_retries:
                    await asyncio.sleep(1)

        click.echo(
            f"  Warning: Could not reach {device_id} ({ip_address}) "
            f"after {max_retries} attempts: {last_error}",
            err=True,
        )
        return DeviceInfo(
            device_id=device_id,
            ip_address=ip_address,
            generation=DeviceGeneration.GEN1,
            reachable=False,
        )

    async def query_by_ip(self, ip_addresses: list[str]) -> list[DeviceInfo]:
        """Query devices directly by IP, bypassing mDNS discovery."""
        click.echo(f"Querying {len(ip_addresses)} device(s) by IP...")
        async with _make_http_client() as http:
            tasks = [self._query_device(ip, ip, http) for ip in ip_addresses]
            results = await asyncio.gather(*tasks, return_exceptions=True)

        devices = []
        for result in results:
            if isinstance(result, Exception):
                click.echo(f"  Warning: Device query failed: {result}", err=True)
            else:
                devices.append(result)

        return devices

    async def scan_devices(
        self, target_device: Optional[str] = None
    ) -> list[DeviceInfo]:
        """Discover and query all Shelly devices (or a specific one)."""
        click.echo("Scanning for Shelly devices...")
        discovered = await discover_devices()

        if not discovered:
            click.echo("No Shelly devices found on the network.")
            return []

        click.echo(f"Found {len(discovered)} device(s), querying...")

        if target_device:
            matches = [(did, ip) for did, ip in discovered if did == target_device]
            if not matches:
                click.echo(f"\nDevice '{target_device}' not found. Discovered devices:", err=True)
                for did, ip in discovered:
                    click.echo(f"  - {did} ({ip})", err=True)
                return []
            discovered = matches

        async with _make_http_client() as http:
            tasks = [self._query_device(did, ip, http) for did, ip in discovered]
            results = await asyncio.gather(*tasks, return_exceptions=True)

        devices = []
        for result in results:
            if isinstance(result, Exception):
                click.echo(f"  Warning: Device query failed: {result}", err=True)
            else:
                devices.append(result)

        return devices

    async def _run_action(
        self,
        device: DeviceInfo,
        action_name: str,
        action_fn,
    ) -> bool:
        """Run an action on a single device with error handling."""
        try:
            success = await action_fn()
            if success:
                click.echo(f"  {device.device_id}: {action_name} - OK")
            else:
                click.echo(f"  {device.device_id}: {action_name} - Failed", err=True)
            return success
        except Exception as e:
            click.echo(f"  {device.device_id}: {action_name} - Error: {e}", err=True)
            return False

    async def init_devices(self, devices: list[DeviceInfo]) -> None:
        """Disable cloud, Bluetooth, and WiFi AP on all reachable devices."""
        reachable = [d for d in devices if d.reachable and not d.auth_enabled]
        if not reachable:
            click.echo("No reachable (non-auth) devices to initialise.")
            return

        click.echo(f"\nInitialising {len(reachable)} device(s)...")

        async with _make_http_client() as http:
            for device in reachable:
                resp = await http.get(f"http://{device.ip_address}/shelly")
                shelly_data = resp.json()
                client = self._make_api_client(
                    device.ip_address, device.device_id, shelly_data, http
                )

                tasks = []
                if device.cloud_enabled is not False:
                    tasks.append(self._run_action(device, "Disable cloud", client.disable_cloud))
                if device.bluetooth_enabled is not None and device.bluetooth_enabled is not False:
                    tasks.append(self._run_action(device, "Disable Bluetooth", client.disable_bluetooth))
                if device.wifi_ap_enabled is not False:
                    tasks.append(self._run_action(device, "Disable WiFi AP", client.disable_wifi_ap))
                if device.is_dimmer:
                    tasks.append(self._run_action(
                        device, "Set transition time 1.0s",
                        lambda c=client: c.set_transition_time(1.0),
                    ))

                if tasks:
                    await asyncio.gather(*tasks)
                else:
                    click.echo(f"  {device.device_id}: Already configured")

    async def calibrate_devices(self, devices: list[DeviceInfo]) -> None:
        """Run calibration on dimmer devices."""
        dimmers = [d for d in devices if d.is_dimmer and d.reachable and not d.auth_enabled]
        if not dimmers:
            click.echo("No reachable dimmers found to calibrate.")
            return

        click.echo(f"\nCalibrating {len(dimmers)} dimmer(s)...")

        async with _make_http_client() as http:
            async def _calibrate(device: DeviceInfo):
                resp = await http.get(f"http://{device.ip_address}/shelly")
                shelly_data = resp.json()
                client = self._make_api_client(
                    device.ip_address, device.device_id, shelly_data, http
                )
                await self._run_action(device, "Calibrate", client.calibrate)

            tasks = [_calibrate(d) for d in dimmers]
            await asyncio.gather(*tasks, return_exceptions=True)

    async def update_devices(self, devices: list[DeviceInfo]) -> None:
        """Trigger firmware update on devices with available updates."""
        updatable = [
            d for d in devices
            if d.reachable and not d.auth_enabled and d.update_available
        ]
        if not updatable:
            click.echo("No devices with available updates.")
            return

        click.echo(f"\nUpdating {len(updatable)} device(s)...")

        async with _make_http_client() as http:
            async def _update(device: DeviceInfo):
                resp = await http.get(f"http://{device.ip_address}/shelly")
                shelly_data = resp.json()
                client = self._make_api_client(
                    device.ip_address, device.device_id, shelly_data, http
                )
                await self._run_action(device, "Firmware update", client.trigger_update)

            tasks = [_update(d) for d in updatable]
            await asyncio.gather(*tasks, return_exceptions=True)
