Shelly Device Management
========================

CLI tool for discovering, auditing, and configuring Shelly smart home devices on the local network.
Supports both Gen1 and Gen2 device APIs.

Dependencies
------------
Requires Python 3.10+ and the following packages:

    pip install click rich httpx zeroconf

Usage
-----
Run from the `shelly/src/` directory:

    python shelly.py [OPTIONS]

### Scan & Audit

With no flags, the tool discovers all Shelly devices on the local network via mDNS and displays an
audit table showing their configuration state.

    python shelly.py

The table shows:

| Column      | Good State     | Bad State        |
|-------------|----------------|------------------|
| Cloud       | Off (green)    | On (red)         |
| Bluetooth   | Off (green)    | On (red)         |
| WiFi AP     | Off (green)    | On (red)         |
| Calibration | OK (green)     | Required (red)   |
| Update      | Up to date     | Available        |

Gen1 devices show "N/A" for Bluetooth (no BT hardware). Non-dimmer devices show "N/A" for
calibration. Auth-protected devices are flagged but cannot be configured.

### Target a Specific Device

    python shelly.py -d <device_id>

The device ID is the mDNS service name (e.g. `shellydimmer2-XXXXXXXXXXXX`). If the device isn't
found, the tool lists all discovered devices so you can check the ID.

### Initialise Devices

    python shelly.py --init

Performs the following on all reachable, non-auth devices:

 * Disables cloud connectivity
 * Disables Bluetooth (Gen2 only)
 * Disables WiFi access point
 * Sets transition time to 1.0 seconds (dimmers only)

Can be combined with `-d` to target a single device:

    python shelly.py -d <device_id> --init

### Calibrate Dimmers

    python shelly.py --calibrate

Runs calibration on all reachable dimmer devices. Only applies to devices identified as dimmers
(Gen1 `SHDM-*` types, Gen2 dimmer models).

### Firmware Updates

    python shelly.py --update

Triggers a firmware update on any device that reports an available update. The device handles the
update process itself — the tool just sends the trigger.

### Combining Flags

Flags can be combined freely. They execute in order: init, calibrate, update. After any actions
are performed, the tool re-scans and displays the updated table.

    python shelly.py --init --calibrate --update
    python shelly.py -d <device_id> --init --update

Architecture
------------

### File Structure

    shelly/src/
        shelly.py               # CLI entry point (Click)
        shelly/
            __init__.py
            device.py           # DeviceInfo dataclass, enums
            discovery.py        # mDNS discovery via zeroconf
            actions.py          # DeviceManager orchestration
            display.py          # Rich table output
            api/
                __init__.py
                base.py         # Abstract API client (ABC)
                gen1.py         # Gen1 HTTP REST client
                gen2.py         # Gen2 JSON-RPC client

### Gen1 vs Gen2 Detection

All Shelly devices respond to `GET /shelly`. Gen2 devices include `"gen": 2` in the response body.
Absence of the `gen` field indicates Gen1. The tool probes this endpoint to determine which API
client to use.

### Gen1 API

Uses HTTP REST endpoints:

 * `GET /shelly` — device identification
 * `GET /settings` — cloud, WiFi AP, input mode, name
 * `GET /status` — update availability, calibration status
 * `POST /settings/cloud` — enable/disable cloud
 * `POST /settings/ap` — enable/disable WiFi AP
 * `POST /settings/light/0` — set transition time
 * `GET /ota?update=true` — trigger firmware update
 * `GET /light/0?calibrate=true` — run dimmer calibration

### Gen2 API

Uses JSON-RPC via `POST /rpc`:

 * `Shelly.GetConfig` — bulk configuration (cloud, BLE, WiFi, input)
 * `Shelly.GetStatus` — device status
 * `Shelly.CheckForUpdate` — firmware update availability
 * `Cloud.SetConfig` — cloud enable/disable
 * `BLE.SetConfig` — Bluetooth enable/disable
 * `WiFi.SetConfig` — WiFi AP enable/disable
 * `Light.SetConfig` — transition time
 * `Shelly.Update` — trigger firmware update
 * `Light.Calibrate` — run dimmer calibration

### Discovery

Uses mDNS to browse `_shelly._tcp.local.` services. The blocking zeroconf library is run via
`asyncio.to_thread()` with a 5-second scan timeout. All subsequent device queries run concurrently
via `asyncio.gather()`.

### Error Handling

 * Unreachable devices are shown greyed out in the table
 * Auth-protected devices are detected and reported (no actions attempted)
 * Per-device errors are caught and reported without aborting the scan
 * 5-second timeout on all HTTP requests
 * Missing JSON fields default to `None` via `.get()`

Troubleshooting
---------------

**No devices found**: Ensure you're on the same network/VLAN as the Shelly devices. mDNS
discovery requires multicast traffic to reach your machine.

**Auth-protected devices**: Devices with authentication enabled will be detected but cannot be
configured. Disable auth via the Shelly app or web UI first, then re-run.

**Timeout errors**: Individual devices may be slow to respond. The 5-second per-request timeout
is fixed. If a device is consistently unreachable, check its network connectivity.
