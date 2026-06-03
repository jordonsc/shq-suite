# Home Assistant Custom Components

Six custom integrations for Home Assistant.

## Components

| Component | Protocol | Port | Config Type | Description |
|-----------|----------|------|-------------|-------------|
| `shq_display` | WebSocket | 8765 | YAML | Nyx kiosk display control |
| `overwatch` | gRPC | 50051 | YAML | Voice TTS and alarm control |
| `dosa` | WebSocket | 8766 | YAML | Door controller (CNC-driven) |
| `actron_mitm_controller` | WebSocket | 8767 | Config Flow | Actron A/C via local MITM bridge (actron-sniffer ESP32) |
| `centurion` | HTTP REST | — | Config Flow | Centurion garage door |
| `cfa_fire_ban` | HTTP (RSS) | — | YAML | CFA fire ban & danger ratings |

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

## actron_mitm_controller (Actron via local RS485 bridge)

**Entities**: Climate (master unit + 8 zones — zone slots 0..7; rename via the HA UI since zone names aren't on the RS485 bus).

**Config**: UI config flow — host + port (default 8767).

**Communication**: WebSocket to the `actron-sniffer` ESP32 (`ws://<host>:8767`). Push-only — server emits a full `state` snapshot on every change plus a 10 s heartbeat. Commands ack/error by client-supplied `id` with 10 s timeout.

**Architecture**: `DataUpdateCoordinator` driven entirely by push (no polling). Availability flips to unavailable after 30 s without a state message.

**Reconnect logic — three layered detectors** (all needed for ESP32 OTA reboots):
1. **WS-level keepalive** (`ping_interval=20, ping_timeout=20` in `client.connect()`). Detects silent connection death within ~40 s. **Load-bearing**: when the ESP32 reboots it doesn't send a TCP FIN, so without WS pings the client-side socket would linger open indefinitely. The server's 10 s state-push heartbeat is server→client only and doesn't help the client detect a dead server.
2. **Connect timeout** (`CONNECT_TIMEOUT_S = 5`). Wraps `websockets.connect()` in `asyncio.wait_for` so a dead host can't hang a reconnect attempt.
3. **Availability monitor force-reconnect**: when `_monitor_availability` sees `is_available()` go False, it calls `_schedule_reconnect(delay=0)` regardless of whether the disconnect callback fired. Belt-and-braces against keepalive missing a network blip.

`RECONNECT_DELAY_S = 30` between attempts. `_connecting` flag in the coordinator prevents parallel connect tasks (the availability monitor and a `_reconnect_after`-driven connect could otherwise race during the 5 s connect window).

**Key design point — no optimistic state or retry logic in the HA client; write reliability lives in the firmware.** The firmware publishes `<field>_transitioning` values while a write is in flight; the climate entities surface `ws_transitioning if not None else ws_value` per the spec. Retry is firmware-side and class-specific: pulse commands (mode/fan/master setpoint/zone enable) are re-fired up to 6× at 10 s intervals (~60 s give-up); zone setpoints are a persistent INJECT held continuously for a 60 s per-phase grace (no re-fire needed). See `actron-sniffer/CLAUDE.md` → "Write reliability — retry vs. hold". (Earlier iterations used a cloud-API integration with heavy optimistic/retry machinery — see git history for `actron_shq` — that approach is no longer needed now the local control surface exists.)

**Key files**: `client.py` (WebSocket + ack correlation + keepalive), `coordinator.py` (connection + dispatch + availability + reconnect), `config_flow.py` (IP+port form), `climate.py` (master + zone entities), `const.py` (timeouts).

**Out of scope for this integration**: away / turbo / continuous-fan / quiet-mode toggles — these are page-1 command codes still to be mapped on the RS485 bus (see `actron-sniffer/FINDINGS.md` §7).

## HA Server Config

The HA server runs on `redacted.host` at `/etc/hass/`. Its config is split between this repo and the live server:

| Lives in repo | Lives only on server |
|---------------|----------------------|
| `home-assistant/custom_components/` — custom integrations | `automations.yaml`, `scripts.yaml`, `scenes.yaml` (UI-managed) |
| `home-assistant/www/shq-icons.js` — custom icon set | `secrets.yaml` |
| `deploy/config/ha/configuration.yaml` — main HA config (modbus, templates, integrations, sensors), gitignored | `.storage/` (config-flow integrations: Centurion, Actron, SolaX, etc.) |

**Update flow** for anything in the repo:

1. Edit the file locally (e.g. `deploy/config/ha/configuration.yaml` for modbus/template/integration changes).
2. `./setup ha` — rsyncs custom components, `configuration.yaml`, and `www/` to `redacted.host:/etc/hass/`, then triggers a YAML-only reload via the HA REST API. Use `./setup ha --restart` if the change needs a full HA restart (new integration, custom component dependency change, anything that doesn't hot-reload).
3. Verify with `./ha get /api/states/sensor.<thing>` or watch the HA logs.

For automations/scripts/scenes, edit them in the HA UI directly — they live in `automations.yaml` etc. on the server and aren't tracked here.

`secrets.yaml` and the `.storage/` directory are server-side only; never overwrite them via deploy.

**Deploy gotcha — orphan components**: `./setup ha` uses `rsync` without `--delete`, so removing a custom component from the repo doesn't remove it from `/etc/hass/custom_components/` on atlas. The orphan dir keeps existing `.pyc` files and `home-assistant.loader` will continue to discover the integration on each restart (just won't load it without a config entry). To fully remove a component you must (a) `sudo rm -rf /etc/hass/custom_components/<name>` on atlas, plus (b) delete the HA config entry, plus (c) clean up any orphan entities the entity registry restored. Future fix: switch `ha_deployer.py` to `rsync --delete-after` for `custom_components/`.

**Deploy gotcha — HA config files owned by root**: `/etc/hass/automations.yaml`, `configuration.yaml`, etc. are owned by `root:root`, not the SSH user. Direct in-place edits via SSH need `sudo` (passwordless sudo is configured for the deploy user on atlas).

**Deploy gotcha — HA REST API is partial**: `/api/config/automation/config` and `/api/config/entity_registry/*` are NOT exposed over REST. For programmatic edits to those you have to drive the WebSocket API at `/api/websocket` (auth handshake + JSON commands). Examples in this session: removed an orphan entity via `config/entity_registry/remove`, could equally update automations via `config/automation/config/{id}`.

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

## Victron Cerbo GX — Battery Backups (Modbus TCP)

Two Victron MultiPlus-II inverters with battery backup, each fronted by a Cerbo GX polled over Modbus TCP (port 502). The two installs are architecturally identical — same registers, same slave units, same template/automation pattern — they just differ by host and entity prefix.

| Cerbo | Host | Entity prefix | Hub name |
|-------|------|---------------|----------|
| Network | `REDACTED-IP` | `network_battery_` | `cerbo_gx` |
| Study | `REDACTED-IP` | `study_battery_` | `cerbo_gx_study` |

Each battery is wired directly to its Cerbo, so SOC comes from the BMS (slave 225) rather than being estimated from voltage.

**Modbus slaves**:
- `100` — `com.victronenergy.system` (system-level metrics)
- `225` — battery service (BMS)
- `227` — VE.Bus (MultiPlus inverter)

**Sensors** (per Cerbo, swap `<prefix>` for `network_battery_` or `study_battery_`):

| Sensor | Register | Slave | Description |
|--------|----------|-------|-------------|
| `<prefix>voltage` | 840 | 100 | Battery voltage (V) |
| `<prefix>current` | 841 | 100 | Battery current (A, signed) |
| `<prefix>power` | 842 | 100 | Battery power (W, signed) |
| `<prefix>soc` | 843 | 100 | System SOC (%) |
| `<prefix>state` | 844 | 100 | 0=idle, 1=charging, 2=discharging |
| `<prefix>grid_power` | 820 | 100 | Grid input power (W) |
| `<prefix>ac_consumption` | 817 | 100 | AC output consumption (W) |
| `<prefix>switch_position` | 33 | 227 | 1=charger, 2=inverter, 3=on, 4=off |
| `<prefix>grid_lost_alarm` | 64 | 227 | 0=ok, 2=grid lost |
| `<prefix>bms_soc` | 266 | 225 | BMS-reported SOC (%, scale 0.1) |
| `<prefix>ac_input_voltage` | 3 | 227 | Grid voltage (V) |
| `<prefix>ac_input_power` | 12 | 227 | MultiPlus AC input (W) |
| `<prefix>ac_output_power` | 23 | 227 | MultiPlus AC output (W) |
| `<prefix>state_text` | — | — | Template: human-readable state |
| `<prefix>mode` | — | — | Template: human-readable switch position |
| `<prefix>grid_available` | — | — | Template binary: grid_lost_alarm != 2 |

**Automations** (mirrored per battery):
- `<Battery> - Power Outage Detected` — Overwatch warn + PagerDuty trigger when grid drops for 10s
- `<Battery> - Power Restored` — Overwatch notify + PagerDuty resolve when grid returns

**Config**: Modbus hubs defined in `deploy/config/ha/configuration.yaml`. Poll interval: 10s.

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
