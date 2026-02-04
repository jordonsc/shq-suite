"""Data models for Shelly devices."""

from dataclasses import dataclass, field
from enum import Enum
from typing import Optional


class DeviceGeneration(Enum):
    GEN1 = 1
    GEN2 = 2


class InputMode(Enum):
    TOGGLE = "toggle"
    EDGE = "edge"
    DETACHED = "detached"
    BUTTON = "button"
    ANALOG = "analog"
    UNKNOWN = "unknown"


@dataclass
class DeviceInfo:
    device_id: str
    ip_address: str
    generation: DeviceGeneration
    model: str = "Unknown"
    name: str = ""
    firmware_version: str = ""
    auth_enabled: bool = False
    cloud_enabled: Optional[bool] = None
    bluetooth_enabled: Optional[bool] = None
    wifi_ap_enabled: Optional[bool] = None
    needs_calibration: Optional[bool] = None
    input_modes: dict[int, InputMode] = field(default_factory=dict)
    transition_time: Optional[float] = None
    update_available: Optional[bool] = None
    is_dimmer: bool = False
    reachable: bool = True
