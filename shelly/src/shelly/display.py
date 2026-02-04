"""Rich table output for Shelly device information."""

from rich.console import Console
from rich.table import Table

from shelly.device import DeviceInfo, InputMode


def _status_cell(value: bool | None, good_value: bool, good_text: str, bad_text: str) -> str:
    """Format a boolean status cell with colour."""
    if value is None:
        return "[dim]N/A[/dim]"
    if value == good_value:
        return f"[green]{good_text}[/green]"
    return f"[red]{bad_text}[/red]"


def display_devices(devices: list[DeviceInfo]) -> None:
    """Print a Rich table of device information."""
    console = Console()

    if not devices:
        console.print("[yellow]No devices to display.[/yellow]")
        return

    table = Table(title="Shelly Devices", show_lines=True)
    table.add_column("Name / ID", style="bold")
    table.add_column("IP Address")
    table.add_column("Gen")
    table.add_column("Cloud")
    table.add_column("Bluetooth")
    table.add_column("WiFi AP")
    table.add_column("Calibration")
    table.add_column("Transition")
    table.add_column("Input Mode")
    table.add_column("Update")

    for device in sorted(devices, key=lambda d: d.name or d.device_id):
        if not device.reachable:
            table.add_row(
                f"[dim]{device.name or device.device_id}[/dim]",
                f"[dim]{device.ip_address}[/dim]",
                "[dim]?[/dim]",
                "[dim]—[/dim]",
                "[dim]—[/dim]",
                "[dim]—[/dim]",
                "[dim]—[/dim]",
                "[dim]—[/dim]",
                "[dim]—[/dim]",
                "[dim]Unreachable[/dim]",
            )
            continue

        if device.auth_enabled:
            table.add_row(
                device.name or device.device_id,
                device.ip_address,
                str(device.generation.value),
                "[yellow]Auth[/yellow]",
                "[yellow]Auth[/yellow]",
                "[yellow]Auth[/yellow]",
                "[yellow]Auth[/yellow]",
                "[yellow]Auth[/yellow]",
                "[yellow]Auth[/yellow]",
                "[yellow]Auth[/yellow]",
            )
            continue

        name = device.name or device.device_id

        # Cloud: Off = good (green), On = bad (red)
        cloud = _status_cell(device.cloud_enabled, False, "Off", "On")

        # Bluetooth: N/A for Gen1, Off = good, On = bad
        bt = _status_cell(device.bluetooth_enabled, False, "Off", "On")

        # WiFi AP: Off = good, On = bad
        wifi_ap = _status_cell(device.wifi_ap_enabled, False, "Off", "On")

        # Calibration: N/A for non-dimmers
        if not device.is_dimmer:
            calibration = "[dim]N/A[/dim]"
        elif device.needs_calibration:
            calibration = "[red]Required[/red]"
        elif device.needs_calibration is False:
            calibration = "[green]OK[/green]"
        else:
            calibration = "[dim]N/A[/dim]"

        # Transition time: N/A for non-dimmers
        if not device.is_dimmer:
            transition = "[dim]N/A[/dim]"
        elif device.transition_time is not None:
            transition = f"{device.transition_time:.1f}s"
        else:
            transition = "[dim]N/A[/dim]"

        # Input modes
        if not device.input_modes:
            input_mode = "[dim]N/A[/dim]"
        elif len(device.input_modes) == 1:
            mode = next(iter(device.input_modes.values()))
            input_mode = "[dim]Unknown[/dim]" if mode == InputMode.UNKNOWN else mode.value.title()
        else:
            parts = []
            for idx in sorted(device.input_modes):
                mode = device.input_modes[idx]
                label = "[dim]?[/dim]" if mode == InputMode.UNKNOWN else mode.value.title()
                parts.append(f"{idx}: {label}")
            input_mode = " / ".join(parts)

        # Update
        if device.update_available is None:
            update = "[dim]N/A[/dim]"
        elif device.update_available:
            update = "[yellow]Available[/yellow]"
        else:
            update = "[green]Up to date[/green]"

        table.add_row(
            name,
            device.ip_address,
            str(device.generation.value),
            cloud,
            bt,
            wifi_ap,
            calibration,
            transition,
            input_mode,
            update,
        )

    console.print(table)
