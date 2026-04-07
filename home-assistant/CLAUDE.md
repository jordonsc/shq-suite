# Home Assistant Custom Components

Six custom integrations for Home Assistant.

## Components

| Component | Protocol | Port | Config Type | Description |
|-----------|----------|------|-------------|-------------|
| `shq_display` | WebSocket | 8765 | YAML | Nyx kiosk display control |
| `overwatch` | gRPC | 50051 | YAML | Voice TTS and alarm control |
| `dosa` | WebSocket | 8766 | YAML | Door controller (CNC-driven) |
| `centurion` | HTTP REST | — | Config Flow | Centurion garage door |
| `cfa_fire_ban` | HTTP (RSS) | — | YAML | CFA fire ban & danger ratings |
| `actron_shq` | Cloud API | — | Config Flow | Actron air conditioning control |

## shq_display (Nyx Kiosk Control)

**Entities per device**: Light (brightness), Sensors (version, URL), Numbers (dim/bright levels, dim/off times)

**Services**: `shq_display.navigate` — navigate kiosk Chrome to a URL

**Config**:
```yaml
shq_display:
  kiosk_name:
    host: 192.168.x.x
    port: 8765
    name: "Friendly Name"
```

**Architecture**: Coordinator pattern with WebSocket. Real-time metrics via broadcast, 30s availability timeout, auto-reconnect with 5s delay.

**Key files**: `client.py` (WebSocket), `coordinator.py` (HA coordinator), `light.py`, `sensor.py`, `number.py`

## overwatch (Voice/TTS)

**Entities**: None (service-only integration)

**Services**:
- `overwatch.set_alarm` — start/stop alarm loop (`alarm_id`, `enabled`, `volume?`)
- `overwatch.verbalise` — TTS speech (`text`, `notification_tone_id?`, `voice_id?`, `volume?`)

**Config**:
```yaml
overwatch:
  host: 192.168.x.x
  port: 50051
```

**Key files**: `client.py` (gRPC), `proto/` (generated stubs, symlink to `overwatch/proto/voice.proto`)

To regenerate proto stubs: `cd proto && pip install grpcio-tools && ./generate.sh`

## dosa (Door Controller)

**Entities per device**: Cover (door open/close/stop/position), Buttons (home, zero, clear_alarm)

**Services**: `dosa.jog` — relative movement in mm

**Config**:
```yaml
dosa:
  device_id:
    host: 192.168.x.x
    port: 8766
    name: "Door Name"
```

**Architecture**: Same coordinator pattern as shq_display. Cover supports OPEN, CLOSE, STOP, SET_POSITION.

**Key files**: `client.py` (WebSocket), `coordinator.py`, `cover.py`, `button.py`

## centurion (Garage Door)

**Entities**: Cover (door), Switches (lamp, vacation mode)

**Config**: UI config flow — prompts for IP address and API key

**Communication**: Simple HTTP GET with query params (`?key={api_key}&door=open`). All entities have availability tracking — go unavailable when the controller is unreachable, recover automatically on next successful poll.

**Key files**: `config_flow.py`, `cover.py`, `switch.py`

## cfa_fire_ban (CFA Fire Ban)

**Entities**: Binary sensor (Total Fire Ban on/off), Sensor (Fire Danger Rating)

**Config**:
```yaml
cfa_fire_ban:
  district: central    # optional, default central
```

**Architecture**: `DataUpdateCoordinator` polling CFA RSS feed every 30 min. Parses XML for TFB status and fire danger rating.

**Key files**: `const.py` (districts), `coordinator.py` (RSS fetch/parse), `binary_sensor.py`, `sensor.py`

## actron_shq (Actron Air Conditioning)

**Entities**: Climate (main unit + per-zone), Sensors (outdoor temp, humidity)

**Config**: UI config flow — OAuth2 device-code authentication

**Communication**: Cloud API via `actron-neo-api` SDK, polled every 60s

**Architecture**: `DataUpdateCoordinator` with fault-tolerant API wrapper (exponential backoff, auth retry, 60s timeout). Config stores only `refresh_token`.

**Key files**: `api.py` (fault-tolerant wrapper), `coordinator.py` (polling), `config_flow.py` (device-code OAuth2), `climate.py` (main + zone entities), `sensor.py` (outdoor temp, humidity)

**Climate features**: HVAC modes (off/cool/heat/auto/fan_only), fan modes (low/medium/high/auto), target temperature. Zones support on/off and target temperature only.

## Home Assistant REST API

A helper script `./ha` in the project root wraps the HA REST API with authentication. It uses `$HA_URL` and `$HA_TOKEN` env vars (set in `~/.bashrc`).

```bash
# Usage: ./ha <get|post> <path> [json_body]

# List all entity states
./ha get /api/states

# Get a single entity state
./ha get /api/states/light.living_downlights

# List all automations (config)
./ha get /api/config/automation/config

# Get a specific automation config
./ha get /api/config/automation/config/{id}

# List all scripts/scenes (config)
./ha get /api/config/script/config
./ha get /api/config/scene/config

# Call a service
./ha post /api/services/light/turn_on '{"entity_id": "light.living_downlights"}'

# Filter entities by domain
./ha get /api/states | \
  python3 -c "import json,sys; [print(e['entity_id'],e['state']) for e in json.load(sys.stdin) if e['entity_id'].startswith('light.')]"
```

### MCP Server (home-assistant)

An MCP server is configured in `~/.claude.json` for basic voice-assistant-style control (turn on/off, set temperature, media, etc). Useful for quick device control but **does not** support listing entities, reading automations, or managing config — use the REST API for those.

## Victron Cerbo GX — Network Battery (Modbus TCP)

A Victron MultiPlus-II with battery backup for network infrastructure, monitored via a Cerbo GX at `REDACTED-IP` using Modbus TCP (port 502). The battery is connected to the MultiPlus, not the Cerbo directly — so no BMS-reported SOC; it's estimated from voltage.

**Modbus units**: Unit 100 = system-level, Unit 227 = VE.Bus (MultiPlus)

**Entities** (all prefixed `network_battery_`, configured in `configuration.yaml`):

| Sensor | Register | Unit | Description |
|--------|----------|------|-------------|
| `voltage` | 840 | 100 | Battery voltage (V) |
| `current` | 841 | 100 | Battery current (A, signed) |
| `power` | 842 | 100 | Battery power (W, signed) |
| `state` | 844 | 100 | 0=idle, 1=charging, 2=discharging |
| `grid_power` | 820 | 100 | Grid input power (W) |
| `ac_consumption` | 817 | 100 | AC output consumption (W) |
| `ac_input_voltage` | 3 | 227 | Grid voltage (V) |
| `ac_input_power` | 12 | 227 | MultiPlus AC input (W) |
| `ac_output_power` | 23 | 227 | MultiPlus AC output (W) |
| `soc_estimate` | — | — | Template: voltage-based SOC estimate (44V=0%, 55.2V=100%) |
| `grid_available` | — | — | Template: binary sensor from grid_power > 0 |

**Automations**:
- `Network Battery - Power Outage Detected` — Overwatch warn announcement + PagerDuty alert when grid drops for 10s
- `Network Battery - Power Restored` — Overwatch notify announcement + PagerDuty resolve when grid returns

**Config**: Modbus sensors defined in `deploy/config/ha/configuration.yaml` (gitignored). Poll interval: 10s.

## Custom Icons (`www/shq-icons.js`)

Custom SVG icon set for HA, registered as `shq:` prefix (e.g. `shq:floor-lamp`).

**File**: `home-assistant/www/shq-icons.js` — deployed to `/etc/hass/www/` via `./setup ha`

**Adding a new icon**:
1. Design or source an SVG at 24x24 viewBox
2. Extract the `d` attribute from the `<path>` element
3. Add an entry to `SHQ_ICONS` in `shq-icons.js` with a `path` key (and optional `viewBox` if not 24x24)
4. Deploy: `./setup ha`
5. Cache-bust the Lovelace resource so browsers pick up the change:
```python
python3 << 'EOF'
import asyncio, json, os, time
async def main():
    import websockets
    url = os.environ["HA_URL"].replace("http", "ws") + "/api/websocket"
    async with websockets.connect(url) as ws:
        await ws.recv()
        await ws.send(json.dumps({"type": "auth", "access_token": os.environ["HA_TOKEN"]}))
        await ws.recv()
        await ws.send(json.dumps({
            "id": 1,
            "type": "lovelace/resources/update",
            "resource_id": "e54a34a4b1de4e8d8090d25306468adb",
            "url": f"/local/shq-icons.js?v={int(time.time())}",
        }))
        print(await ws.recv())
asyncio.run(main())
EOF
```

**Icon format**: HA renders icons as filled SVG paths (not stroked). The `d` value must define closed filled shapes, not stroke outlines. All icons use `fill` — `stroke` attributes are ignored.

**Lovelace resource**: Registered once via Settings → Dashboards → Resources as `/local/shq-icons.js` (type: JavaScript Module). The resource ID `e54a34a4b1de4e8d8090d25306468adb` is used for cache-busting updates.

## Common Patterns

- WebSocket integrations (`shq_display`, `dosa`) share a coordinator pattern with:
  - Persistent WebSocket connection with keepalive
  - Real-time state broadcasts from the server
  - Reconnection with backoff on disconnect
  - Availability tracking (30s timeout)
- YAML-configured integrations use dictionary keys as device IDs
- All deps declared in `manifest.json` per component
- HA deploys to `redacted.host` via `./setup ha`
