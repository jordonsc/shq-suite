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
| `somfy_sdn` | WebSocket | 8767 | Config Flow | Somfy SDN blind motors via the somfy-sdn ESP32 (one `cover` per motor) |
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
- `overwatch.play_tone` — play a single tone, no TTS (`tone_id`, `volume?`); `tone_id` is a key from the server's `notification_tones` config

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

**Entities**: Climate (master unit + 8 zones — zone slots 0..7; rename via the HA UI since zone names aren't on the RS485 bus). The master exposes a current temperature (indoor-unit main/return-air reading scraped from bus reg 13, published as the master's `current_temp`), so its thermostat card shows current + setpoint like the zones do.

**`hvac_action` (climate-card haze)**: derived from the *mode* only — we don't read compressor demand off the bus (`climate.py:_action_for_mode`). heat→`heating`, cool→`cooling`, fan_only→`fan`, off→`off`, heat_cool→resolved by current-vs-target (else `idle`). Zones mirror the master's action, or `off` when disabled. This is what drives the coloured background animation on the thermostat/tile card (amber=heating, blue=cooling/fan); without it the card stays flat.

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

## somfy_sdn (Somfy SDN blind motors)

**Entities**: one `cover.somfy_<addr>` per motor (device class *shade*; OPEN/CLOSE/STOP/SET_POSITION). Entities are created **dynamically** as motors appear in the firmware's state payload (configured / discovered / passively observed). Per-motor `available` follows the device's `online` flag, so a single dropped motor goes unavailable without taking the whole bridge down.

**Position inversion**: the firmware reports **native Somfy %** (0 = open, 100 = closed); the entity maps `current_cover_position = 100 − somfy_pct` and `set_position` sends the HA position (firmware inverts). One place only — mirror of `sdn::haToSomfy` in firmware.

**Entities** (platforms: cover, button, switch, number, sensor, binary_sensor — shared base in `entity.py`). A **controller device** per ESP32 (buttons: Rediscover motors, Reconnect WiFi (`reconnect_wifi` admin command — re-scan + reassociate to the strongest AP, for moving off a distant AP the device fell back to at boot); switch: Bus active = ACTIVE/LISTEN; diagnostic sensors: motors-online, wire-errors) and a **per-motor device** alongside each cover holding `EntityCategory.CONFIG`/`DIAGNOSTIC` calibration entities (switch: Reversed; binary_sensor: Fault; numbers: Jog duration + Bottom limit (pulses, stateful read/write); buttons: Set top/bottom limit, Identify, Reset positions, Jog up/down — Jog fires a timed CTRL_MOVE nudge that works before limits). Devices are deletable from the UI (`async_remove_config_entry_device` forgets them on the controller). Config-category = they live on the device page, not dashboards. New motors auto-create their device when they appear in state (after a rediscover).

**Services** (same ops, scriptable for automations; target `entity_id`): `somfy_sdn.move_steps {direction, pulses}`, `set_top_limit`, `set_bottom_limit`, `set_direction {reversed}`, `reset`, `identify`, `set_mode {listen|active}`. Some calibration payloads are pending a Set Pro capture (see `somfy-sdn/CLAUDE.md`).

**Config / comms / reconnect**: identical pattern to `actron_mitm_controller` — push-only `DataUpdateCoordinator`, the same three-layer reconnect (`client.py`/`coordinator.py` ported near-verbatim). No optimistic state: `ack` means the command was queued; motor-confirmed state arrives via the next snapshot. Firmware: `somfy-sdn/`.

**Address discovery (zeroconf, self-healing)**: unlike the Actron controller (manual IP / DHCP reservation), this integration auto-discovers the device. The firmware advertises `_somfy-sdn._tcp` with TXT `id=<MAC>`; `config_flow.async_step_zeroconf` keys the config entry on that MAC and rewrites the stored host to the current IP on every re-announcement, so a reboot onto a new DHCP lease just works — no manual IP, no router reservation. Manual host+port (default 8767) is still offered as a fallback. Verified live on the LAN.

**Controller device naming & web link**: the controller device is named `Somfy SDN (<MAC>)` — `coordinator.mac` is seeded from the entry's MAC `unique_id` and refreshed from the WS `state.mac` field, so the name is IP-independent (doesn't churn on DHCP changes; the old `Somfy SDN (<ip>)` did). `DeviceInfo.configuration_url=http://<host>/` gives a **"Visit"** link on the device page to the firmware's HTML dashboard; it follows the self-healed IP.

**MAC-keyed identity (config entry v2)**: `entity.controller_id()` returns `coordinator.controller_key` = the controller MAC (bare hex, e.g. `404cca512e64`), so every entity `unique_id` and device identifier survives a DHCP IP change — was `f"{host}:{port}"` (v1), which re-keyed/orphaned everything on an IP change. `controller_key` is fixed at coordinator init from the entry's MAC `unique_id` (falls back to `host:port` for manual entries with no discovered MAC). Identifier shapes: controller = `<mac>`, motor = `<mac>:AA:BB:CC`. `async_migrate_entry` (v1→v2, `__init__.py`) rewrites existing registry unique_ids + device identifiers in place (history/customisations preserved) and renames the IP-bearing controller entity_ids to a MAC slug (`somfy_sdn_40_4c_ca_51_2e_64_*`); motor entity_ids were already addr-based. Verified live: 3 entries migrated, 0 orphans, a renamed motor ("Gym Blinds") survived. **Bump `ConfigFlow.VERSION` + extend the migration if the identifier scheme changes again.**

**Key files**: `client.py`, `coordinator.py` (`controller_key`, `_mac_norm`/`_format_mac`), `config_flow.py` (`VERSION`), `__init__.py` (`async_migrate_entry`), `entity.py` (shared bases + controller `device_info`), `cover.py` (per-motor entity + entity services), `services.yaml`, `const.py`.

## HA Server Config

The HA server runs on `redacted.host` at `/etc/hass/`. Its config is split between this repo and the live server:

| Lives in repo | Lives only on server |
|---------------|----------------------|
| `home-assistant/custom_components/` — custom integrations | `automations.yaml`, `scripts.yaml`, `scenes.yaml` (UI-managed) |
| `home-assistant/www/shq-icons.js` — custom icon set | `secrets.yaml` |
| `home-assistant/blueprints/` — automation blueprints (→ `/etc/hass/blueprints/`) | `.storage/` (config-flow integrations: Centurion, Actron, SolaX, etc.) |
| `deploy/config/ha/configuration.yaml` — main HA config (modbus, templates, integrations, sensors), gitignored | |

**Update flow** for anything in the repo:

1. Edit the file locally (e.g. `deploy/config/ha/configuration.yaml` for modbus/template/integration changes).
2. `./setup ha` — rsyncs custom components, `configuration.yaml`, and `www/` to `redacted.host:/etc/hass/`, then triggers a YAML-only reload via the HA REST API. Use `./setup ha --restart` if the change needs a full HA restart (new integration, custom component dependency change, anything that doesn't hot-reload).
3. Verify with `./ha get /api/states/sensor.<thing>` or watch the HA logs.

For automations/scripts/scenes, edit them in the HA UI directly — they live in `automations.yaml` etc. on the server and aren't tracked here.

**Blueprints** (`home-assistant/blueprints/automation/shq/`): version-controlled here, deployed by `./setup ha`. Current:

- `exhaust_fan.yaml` — light-linked exhaust fan. Inputs: trigger lights (all must be off to stop), fan switch, auto-on toggle (default on; disabled for manually-started fans), on-delay (default 3 min), off-delay (default 5 min), max runtime (default 1 h). Includes an HA-start safety check that turns off a fan left running with all lights off (covers `for:` countdowns lost to a restart). Instances: "Powder Room Fan", "Ensuite 1 Toilet Fan", "Ensuite 1 Shower Fan" (auto-on off, 10 min off-delay).

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
