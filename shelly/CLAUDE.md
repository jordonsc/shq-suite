# Shelly Device Management Tool

Python CLI for discovering, auditing, and configuring Shelly smart home devices on the local network. Supports both Gen1 (HTTP REST) and Gen2 (JSON-RPC) APIs.

## Usage

```bash
cd shelly/src
python shelly.py                    # Scan and display all devices
python shelly.py --init             # Initialise devices (disable cloud/BT/AP, set transitions)
python shelly.py -d <id> --init     # Target a specific device
python shelly.py --calibrate        # Calibrate dimmers
python shelly.py --update           # Trigger firmware updates
```

## Source Layout

```
shelly/src/
├── shelly.py                    # CLI entry point (asyncio + argparse)
└── shelly/
    ├── __init__.py
    ├── device.py                # DeviceInfo dataclass, DeviceGeneration enum, InputMode enum
    ├── discovery.py             # mDNS discovery via zeroconf (_http._tcp.local.)
    ├── display.py               # Rich console table output
    ├── initialiser.py           # Init logic: disable cloud, BT, AP; set transitions
    └── api/
        ├── __init__.py
        ├── base.py              # Abstract ShellyAPIClient base class
        ├── gen1.py              # Gen1 client (HTTP REST: /settings, /status, /shelly)
        └── gen2.py              # Gen2 client (JSON-RPC via /rpc endpoint)
```

## Architecture

1. **Discovery**: Uses zeroconf to find `_http._tcp.local.` services, identifies Shelly devices by hostname pattern (`shelly*` or `Shelly*`)
2. **Generation detection**: Probes `/shelly` endpoint — Gen2 has `gen` field, Gen1 doesn't
3. **API abstraction**: Both generations implement `ShellyAPIClient` with common methods: `get_device_info()`, `disable_cloud()`, `disable_wifi_ap()`, `disable_bluetooth()`, `set_transition_time()`, `trigger_update()`, `calibrate()`
4. **Display**: Rich library for coloured table output with device status

## Initialisation (`--init`)

Disables unnecessary features for local-only operation:
- Cloud connectivity
- Bluetooth
- WiFi AP mode
- Sets dimmer transition time to 0.5s

## Dependencies

Uses `httpx` (async HTTP), `zeroconf` (mDNS), `rich` (terminal output). No requirements.txt — install manually.

## Key Types

- `DeviceGeneration`: GEN1 or GEN2
- `InputMode`: TOGGLE, EDGE, DETACHED, BUTTON, UNKNOWN
- `DeviceInfo`: All device metadata (model, firmware, cloud/BT/AP status, calibration, etc.)
