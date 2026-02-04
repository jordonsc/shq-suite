#!/usr/bin/env python3
"""
Shelly Device Management CLI

Discover, audit, and configure Shelly smart home devices on the local network.
"""

import asyncio
import sys

import click

from shelly.actions import DeviceManager
from shelly.display import display_devices


async def run(
    device: str | None,
    ip: tuple[str, ...],
    init: bool,
    calibrate: bool,
    update: bool,
) -> int:
    manager = DeviceManager()

    if ip:
        devices = await manager.query_by_ip(list(ip))
    else:
        devices = await manager.scan_devices(target_device=device)

    if not devices:
        return 1 if (device or ip) else 0

    display_devices(devices)

    if init:
        await manager.init_devices(devices)

    if calibrate:
        await manager.calibrate_devices(devices)

    if update:
        await manager.update_devices(devices)

    # Re-query to verify changes
    if init or calibrate or update:
        click.echo("\nRe-scanning to verify changes...")
        if ip:
            devices = await manager.query_by_ip(list(ip))
        else:
            devices = await manager.scan_devices(target_device=device)
        if devices:
            display_devices(devices)

    return 0


@click.command()
@click.option(
    "--device",
    "-d",
    default=None,
    help="Target a specific device by ID (requires mDNS).",
)
@click.option(
    "--ip",
    multiple=True,
    help="Query device(s) by IP address directly, bypassing mDNS. Can be specified multiple times.",
)
@click.option(
    "--init",
    is_flag=True,
    help="Disable cloud, Bluetooth, and WiFi AP on devices.",
)
@click.option(
    "--calibrate",
    is_flag=True,
    help="Run calibration on dimmer devices.",
)
@click.option(
    "--update",
    is_flag=True,
    help="Trigger firmware update on devices with available updates.",
)
def cli(device: str | None, ip: tuple[str, ...], init: bool, calibrate: bool, update: bool):
    """
    Shelly Device Management Tool.

    Discover and audit Shelly devices on the local network.
    Use --init to disable cloud/BT/AP, --calibrate for dimmers,
    and --update to trigger firmware updates.

    Use --ip to bypass mDNS discovery and query devices directly (e.g. from WSL2).
    """
    if device and ip:
        raise click.UsageError("--device and --ip are mutually exclusive.")
    exit_code = asyncio.run(run(device, ip, init, calibrate, update))
    sys.exit(exit_code)


if __name__ == "__main__":
    cli()
