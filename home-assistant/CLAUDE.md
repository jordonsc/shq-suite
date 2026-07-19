# Home Assistant Custom Components

Custom integrations for Home Assistant.

## Components

> ⚠️ **The edge-suite components (`shq_display`, `overwatch`, `argus`) are authoritative in the Argus
> repo** (`jordonsc/argus/home-assistant/custom_components/`) — that's where they're maintained and
> deployed from now. `shq_display`/`overwatch` are currently in sync; the `argus` component is **ahead
> there** (0.3.0 vs 0.1.0 here). Edit/deploy those three from the Argus repo. The rest below
> (`dosa`, `actron_mitm_controller`, `centurion`, `somfy_sdn`, `cfa_fire_ban`) are **owned by this
> shq-suite repo**. See ledger shq-suite-0015.

| Component | Protocol | Port | Config Type | Description |
|-----------|----------|------|-------------|-------------|
| `shq_display` | WebSocket | 8765 | YAML | Nyx kiosk display control · **→ Argus repo** |
| `overwatch` | gRPC | 50051 | YAML | Voice TTS and alarm control · **→ Argus repo** |
| `dosa` | WebSocket | 8766 | YAML | Door controller (CNC-driven) |
| `argus` | WebSocket | 8770 | YAML | AI alarm-assessment status + ack/standdown (Argus daemon on atlas) · **→ Argus repo (ahead: 0.3.0)** |
| `actron_mitm_controller` | WebSocket | 8767 | Config Flow | Actron A/C via local MITM bridge (actron-sniffer ESP32) |
| `centurion` | HTTP REST | — | Config Flow | Centurion garage door |
| `somfy_sdn` | WebSocket | 8767 | Config Flow | Somfy SDN blind motors via the somfy-sdn ESP32 (one `cover` per motor) |
| `cfa_fire_ban` | HTTP (RSS) | — | YAML | CFA fire ban & danger ratings |
| `unifi_access_dps` | WebSocket (wss) | 12445 | YAML | Front-door DPS workaround — raw hub input via the UniFi Access developer websocket |

## shq_display (Nyx Kiosk Control)

**Entities per device**: Light (brightness), Sensors (version, URL), Numbers (dim/bright levels, dim/off times), Select (**Idle Mode**: `off`|`clock`)

**Idle Mode select** (`select.py`, component 1.1.0): `off` = blank the screen at the auto-off timeout (default, all kiosks); `clock` = show the Chronos clock overlay instead, held at `dim_level` brightness (see `chronos/` + `nyx/` "Clock screensaver"). Shows `unknown` for kiosks still on nyx < 1.1.0 (they don't report `idle_mode`). **All entities that call `set_auto_dim_config` (the four Numbers + this Select) pass the current `idle_mode`** — nyx resets an omitted `idle_mode` to `off`, so the Numbers must preserve it.

**Services**: `shq_display.navigate` — navigate kiosk Chrome to a URL. Optional `wake` (bool) + `keep_awake` (bool), component 1.2.0 / nyx ≥ 1.2.0: `wake: true` wakes the backlight + dismisses the clock overlay before navigating (so the page shows on a sleeping/`clock` kiosk); `keep_awake: true` pins the screen on (idle loop won't blank), `false` releases the pin. Both omitted by default and ignored by older nyx — Argus's alarm takeover drives them.

**Config**:
```yaml
shq_display:
  kiosk_name:
    host: 192.168.x.x
    port: 8765
    name: "Friendly Name"
```

**Architecture**: Coordinator pattern with WebSocket. Real-time metrics via broadcast, 30s availability timeout, auto-reconnect with 5s delay.

**Key files**: `client.py` (WebSocket), `coordinator.py` (HA coordinator), `light.py`, `sensor.py`, `number.py`, `select.py` (Idle Mode)

> Entity-id note (2026-06-18): the `kiosk03`/`kiosk04` light/number/version entity_ids had been historically crossed in the registry (kiosk03's entities slugged under `…kiosk_04`, kiosk04's under `…kiosk_04_2`). The unique_ids were always correct; the entity_ids were renamed in place to match (`…kiosk_03` / `…kiosk_04`). Nothing referenced the stale ids. The `url` sensors and the new `select`s were unaffected.

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

## argus (AI Alarm Assessment)

Status + control surface for the **Argus** daemon (`argus/`, runs on atlas). Argus itself watches the HA alarm and drives the assessment; this component is the **reverse channel** — it connects to Argus's control WebSocket and surfaces the live `CaseState` in HA, plus exposes acknowledge/standdown actions.

**Entities**:
- `binary_sensor.argus_active` — a case is in progress (alarm `triggered`/`assessing`)
- `sensor.argus_threat_level`, `sensor.argus_intruder_count`, `sensor.argus_summary`, `sensor.argus_case_id`, `sensor.argus_status` — the current `CaseState` projection
- `button.argus_acknowledge`, `button.argus_standdown` — send the corresponding command back to Argus

**Config** (YAML, like `shq_display`/`dosa`):
```yaml
argus:
  host: 192.168.x.x    # atlas
  port: 8770           # the Argus control/web port (web.bind)
  name: "Argus"
```

**Architecture**: Same `local_push` coordinator pattern as `shq_display`/`dosa` — a control WebSocket to the Argus daemon, full-state push on every `CaseState` change, availability timeout + auto-reconnect. The transport rides the Argus **web port** (default `8770`, the same port that serves the kiosk HUD); the component speaks the control protocol, not the kiosk JSON stream.

**Key files**: `client.py` (WebSocket), `coordinator.py` (push + reconnect + availability), `binary_sensor.py`, `sensor.py`, `button.py`.

> The daemon, its `CaseState` contract, and the control-WS API live in `argus/CLAUDE.md` (owned by the Argus side). Argus runs on **atlas alongside HA** but is a separate process; this component is just the HA-side observability/control mirror.

## centurion (Garage Door)

**Entities**: Cover (door), Switches (lamp, vacation mode)

**Config**: UI config flow — prompts for IP address and API key

**Communication**: Simple HTTP GET with query params (`?key={api_key}&door=open`). All entities have availability tracking — go unavailable when the controller is unreachable, recover automatically on next successful poll. The controller's `door` status strings are verbose (`closed by wifi`, `opening by wifi`, `opened. intruder alert`) — `cover.py` `startswith`-matches them (order matters: `opening` before `open`).

**Motion fast-tracking (cover 1.1.0)** — baseline poll is 10 s (`SCAN_INTERVAL`), but while the door is opening/closing `_track_until_settled` fast-polls every 2 s (60 s bound) until it reaches open/closed. Without it, HA could sit in `opening`/`closing` for up to a full poll interval after the door had physically stopped; because the entity advertises `STOP`, HA core resolves `cover.toggle` to **stop** while it thinks the cover is moving, so an at-rest press from the garage remote (which drives `cover.toggle` via `automation.open_close_garage`) mis-resolved to a no-op STOP and was silently eaten — the "press twice" defect (ledger **shq-suite-0007**). The tracker is kicked from `async_open_cover`/`async_close_cover` (HA-initiated) and from `async_update` when it first observes externally-initiated motion (Centurion app / a directly-paired remote). Re-entrancy-guarded by `self._tracking`.

**Garage remote path**: Merlin remote → Merlin E8003 receiver (relay) → TinyS3 ESP32 on legacy `matter-apps` firmware (Matter-over-WiFi, `app_sensor` BooleanState input) → `binary_sensor.garage_remote_button_1` → `automation.open_close_garage` (disarm alarm, then `cover.toggle`). The remote is purely an *input*; this component does the actuation. `cover.toggle`'s stop-while-moving behaviour is intended (classic open→stop→close cycle) — the fix above is about HA's state tracking reality, not changing that semantics.

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

**Close-before-reconnect (WS-slot-leak fix, `somfy_sdn` 1.4.1 / `actron_mitm_controller` 1.1.1).** `_connect_and_run` now calls `_teardown_connection()` (cancel the reader task + `await client.close()`) **before** every `client.connect()`. Detector 3 force-reconnects whenever availability drops, which can fire while the old socket is still physically alive (the ESP went silent for >`AVAILABILITY_TIMEOUT_S` but the TCP stayed half-open). Previously `connect()` just overwrote `self._ws` and orphaned the old connection — which stayed alive on its own WS keepalive and held one of the firmware's 5 `WebSocketsServer` slots indefinitely. A few stall-recover cycles filled every slot with the integration's own live-but-useless zombies; the ESP then refused all new handshakes (`did not receive a valid HTTP response`) and the device looked dead to HA while HTTP and the device's real work stayed fine. Closing sends a FIN so the firmware reaps the slot. The `_connecting` flag is set during teardown, so the cancel-driven `_on_disconnect` can't queue a rival reconnect. (The somfy firmware also gained a wedge-watchdog reboot as a belt-and-braces backstop — see `somfy-sdn/CLAUDE.md`.)

**Key design point — no optimistic state or retry logic in the HA client; write reliability lives in the firmware.** The firmware publishes `<field>_transitioning` values while a write is in flight; the climate entities surface `ws_transitioning if not None else ws_value` per the spec. Retry is firmware-side and class-specific: pulse commands (mode/fan/master setpoint/zone enable) are re-fired up to 6× at 10 s intervals (~60 s give-up); zone setpoints are a persistent INJECT held continuously for a 60 s per-phase grace (no re-fire needed). See `actron-sniffer/CLAUDE.md` → "Write reliability — retry vs. hold". (Earlier iterations used a cloud-API integration with heavy optimistic/retry machinery — see git history for `actron_shq` — that approach is no longer needed now the local control surface exists.)

**Close-code logging (1.1.2 at WARNING; dialled to INFO in 1.1.3 once the fix was confirmed, 2026-07-19).** `client.run()` logs the WS close code + socket lifetime on every disconnect (was a bland INFO "closed by peer"). Added to diagnose the recurring ~30 s `unavailable` flaps, which were root-caused to **firmware loop-starvation** (the actron-sniffer RS485 bridge task starving the ESP's HTTP+WS loop so it went silent, tripping the 30 s availability timeout) — fixed by a yield budget in `actron-sniffer/src/main.cpp`, verified 0 flaps over 10 h under heating load; see ledger shq-suite-0019. The flaps came through the coordinator availability path, so this branch stays quiet post-fix; it fires when a command hits an already-silent socket (the "no close frame received or sent" toast) or on a genuine close. Kept at INFO as a cheap regression signal — bump back to WARNING if you need it in `/api/error_log`.

**Key files**: `client.py` (WebSocket + ack correlation + keepalive + close-code logging), `coordinator.py` (connection + dispatch + availability + reconnect), `config_flow.py` (IP+port form), `climate.py` (master + zone entities), `const.py` (timeouts).

**Out of scope for this integration**: away / turbo / continuous-fan / quiet-mode toggles — these are page-1 command codes still to be mapped on the RS485 bus (see `actron-sniffer/FINDINGS.md` §7).

## somfy_sdn (Somfy SDN blind motors)

**Entities**: one `cover.somfy_<addr>` per motor (device class *shade*; OPEN/CLOSE/STOP/SET_POSITION). Entities are created **dynamically** as motors appear in the firmware's state payload (configured / discovered / passively observed). Per-motor `available` follows the device's `online` flag, so a single dropped motor goes unavailable without taking the whole bridge down.

**Position inversion**: the firmware reports **native Somfy %** (0 = open, 100 = closed); the entity maps `current_cover_position = 100 − somfy_pct` and `set_position` sends the HA position (firmware inverts). One place only — mirror of `sdn::haToSomfy` in firmware.

**Entities** (platforms: cover, button, switch, number, sensor, binary_sensor — shared base in `entity.py`). A **controller device** per ESP32 (buttons: Rediscover motors, Reconnect WiFi (`reconnect_wifi` admin command — re-scan + reassociate to the strongest AP, for moving off a distant AP the device fell back to at boot); switch: Bus active = ACTIVE/LISTEN; diagnostic sensors: motors-online, wire-errors) and a **per-motor device** alongside each cover holding `EntityCategory.CONFIG`/`DIAGNOSTIC` calibration entities (switch: Reversed; binary_sensor: Fault; numbers: Jog duration + Bottom limit (pulses, stateful read/write); buttons: Set top/bottom limit, Identify, Reset positions, Jog up/down — Jog fires a timed CTRL_MOVE nudge that works before limits). Devices are deletable from the UI (`async_remove_config_entry_device` forgets them on the controller). Config-category = they live on the device page, not dashboards. New motors auto-create their device when they appear in state (after a rediscover).

**Services** (same ops, scriptable for automations; target `entity_id`): `somfy_sdn.move_steps {direction, pulses}`, `set_top_limit`, `set_bottom_limit`, `set_direction {reversed}`, `reset`, `identify`, `set_mode {listen|active}`. Some calibration payloads are pending a Set Pro capture (see `somfy-sdn/CLAUDE.md`).

**Config / comms / reconnect**: identical pattern to `actron_mitm_controller` — push-only `DataUpdateCoordinator`, the same three-layer reconnect (`client.py`/`coordinator.py` ported near-verbatim), **including the close-before-reconnect WS-slot-leak fix** (see the Actron reconnect note above; this integration is where the leak first wedged a controller — `bed_1_blinds_left`'s ESP at the full 5-client cap, 2026-06-14). No optimistic state: `ack` means the command was queued; motor-confirmed state arrives via the next snapshot. Firmware: `somfy-sdn/` (which also has a wedge-watchdog reboot backstop).

**Address discovery (zeroconf, self-healing)**: unlike the Actron controller (manual IP / DHCP reservation), this integration auto-discovers the device. The firmware advertises `_somfy-sdn._tcp` with TXT `id=<MAC>`; `config_flow.async_step_zeroconf` keys the config entry on that MAC and rewrites the stored host to the current IP on every re-announcement, so a reboot onto a new DHCP lease just works — no manual IP, no router reservation. Manual host+port (default 8767) is still offered as a fallback. Verified live on the LAN.

**Controller device naming & web link**: the controller device is named `Somfy SDN (<MAC>)` — `coordinator.mac` is seeded from the entry's MAC `unique_id` and refreshed from the WS `state.mac` field, so the name is IP-independent (doesn't churn on DHCP changes; the old `Somfy SDN (<ip>)` did). `DeviceInfo.configuration_url=http://<host>/` gives a **"Visit"** link on the device page to the firmware's HTML dashboard; it follows the self-healed IP.

**MAC-keyed identity (config entry v2)**: `entity.controller_id()` returns `coordinator.controller_key` = the controller MAC (bare hex, e.g. `404cca512e64`), so every entity `unique_id` and device identifier survives a DHCP IP change — was `f"{host}:{port}"` (v1), which re-keyed/orphaned everything on an IP change. `controller_key` is fixed at coordinator init from the entry's MAC `unique_id` (falls back to `host:port` for manual entries with no discovered MAC). Identifier shapes: controller = `<mac>`, motor = `<mac>:AA:BB:CC`. `async_migrate_entry` (v1→v2, `__init__.py`) rewrites existing registry unique_ids + device identifiers in place (history/customisations preserved) and renames the IP-bearing controller entity_ids to a MAC slug (`somfy_sdn_40_4c_ca_51_2e_64_*`); motor entity_ids were already addr-based. Verified live: 3 entries migrated, 0 orphans, a renamed motor ("Gym Blinds") survived. **Bump `ConfigFlow.VERSION` + extend the migration if the identifier scheme changes again.**

**Key files**: `client.py`, `coordinator.py` (`controller_key`, `_mac_norm`/`_format_mac`), `config_flow.py` (`VERSION`), `__init__.py` (`async_migrate_entry`), `entity.py` (shared bases + controller `device_info`), `cover.py` (per-motor entity + entity services), `services.yaml`, `const.py`.

## unifi_access_dps (Front Door DPS workaround)

**Why it exists**: the UA-Hub-Door-Mini's wire-presence detection false-negatives on the DPS terminals (`wiring_state_d1-dps-pos/neg = off` since 2026-03-11) even though the DPS input itself works — the hub relocks-on-close from it. The Access controller trusts the wiring state, so it publishes `door_position_status = "none"` / `dps_connected: false` on the developer API, and the core `unifi_access` integration's DPS entity (`binary_sensor.front_door_door_position_sensor`) can never update. **UniFi firmware bug — remove this component once Ubiquiti fix wire detection** (watch: `GET /api/v1/developer/doors` returning `open`/`close` instead of `none`). Full history: ledger shq-suite-0020.

**How it works**: connects to the same developer websocket the core integration uses (`wss://<host>:12445/api/v1/developer/devices/notifications`, via the `py-unifi-access` lib — already pinned by core, so no new deps) and reads the **raw hub input** from `access.data.device.update` messages: `data.configs[]` key `input_d1_dps` (`on` = circuit closed = door closed, `off` = open). Updates are push-only; the hub emits them on state changes (every open involves an unlock on a fail-safe maglock, so real usage is fully covered).

**Entities** (per configured door, both `RestoreEntity` — state survives restarts; `unknown` until the first hub event on a fresh install):
- `binary_sensor.<name>_position` (device class *door*, `on` = open) — from the raw hub input. Attributes: `raw_input`, `input_updated_at` (UniFi's config timestamp — unreliable, informational only), `wiring_detected` (the buggy wire-presence flag — flips `true` when Ubiquiti fix it), `ws_connected`, `restored`.
- `binary_sensor.<name>_lock` (device class *lock*, `on` = unlocked) — the maglock relay state, which the controller publishes correctly (`state.lock` in location updates / `door_lock_relay_status` on `GET /doors`) but the core integration exposes no entity for. Fed by websocket location updates **plus a 30s `get_doors` poll** (seeds initial state at startup, covers missed pushes). Only created when the door has a `door_id`. **Relay "locked" ≠ door secured**: it means the magnet is energised — secured = locked AND position closed. When unlocked, the door relaxes slightly ajar (no mechanical latch), so DPS correctly reads open until it's held shut and the magnet re-grabs.

**Service**: `unifi_access_dps.lock_now {device_id}` — sends lock rule `lock_now` (accepted by the API but absent from the core integration's `vol.In` allowlist) to end a held unlock immediately. Kept for manual use; a force-relock automation was built then removed 2026-07-16 — the REX-walk-away case leaves the door ajar, so the *Front Door Open* left-open alert covers it.

**Config** (YAML, in `deploy/config/ha/configuration.yaml`): `host`, `api_token` (`!secret unifi_access_token` — same token as the core integration), optional `verify_ssl` (default false), `doors: [{device_id: <hub MAC-id>, door_id: <Access door/location UUID>, name: <base name>, input_key: input_d1_dps}]`.

**Consumers**: `binary_sensor.front_door_position` drives the *Front Door Open* (left-open alert, id 1769997461752) and *Alarm Sensors* (id 1770875325186) automations plus the dashboard-shq DPS badge — all rewired from the dead core entity 2026-07-16. Swap them back if the core entity revives.

**⚠️ Never remote-unlock a maglock door unattended** (`PUT /doors/{id}/unlock` or otherwise): with no mechanical latch the door can swing open on its own and stand open — the relay re-engaging 10s later holds nothing.

**Key files**: `__init__.py` (YAML schema + setup + service), `coordinator.py` (websocket consumer + lock poll, `UnifiAccessDpsHub`), `binary_sensor.py`, `services.yaml`, `const.py`.

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
- `power_cycle.yaml` — power-cycle on unavailable. Inputs: watched entities (any domain, multiple), power-cycle switch, unavailable grace period (default 5 min), power-off time (default 5 s), notify action (text, empty = no notification). A template condition requires the switch itself to be available before any action — a genuine power outage (which takes the Shelly down too) never trips it. Instances: "Unavailable: Bedroom Lights", "Unavailable: Room B Lights", "Unavailable: WIR1 Light" (all dry-contact Shellys cycling Matter-over-WiFi downlights, notify to redacted_phone).
- `verify_power_on_activate.yaml` — verify a power-metered device actually drew power after turning on; power-cycle it if not. Inputs: target (light/switch), power sensor (W), minimum power (default 5 W), settle delay (default 2 s), power-off time (default 1 s), max cycles (default 3), notify action (empty = none). Triggers on the target reaching `on`; after the settle delay, if draw is below the threshold it does an off→pause→on cycle and re-checks, bounded by max cycles. `mode: single` so the cycle's own off→on (which re-fires the trigger) is dropped mid-run — the bounded `repeat` is the only re-check, so a dead device can't loop forever. Instance: "Void Pendant: verify on activation" (`light.void_pendant` / `sensor.void_pendant_power`, 5 W, notify to redacted_phone) — short-term workaround for the void pendant Shelly dimmer glitch where it reports `on` but the lamp doesn't illuminate (draws ~2 W vs ~17 W healthy at brightness 48); a single cycle reliably recovers it. Caveat: the 5 W floor assumes the lamp is bright enough to clear it — at very low brightness a healthy lamp could legitimately sit under 5 W and trip a spurious cycle.
- `tab_group.yaml` — mutually-exclusive boolean group (radio behaviour) for kiosk UI tab-groups. Single input: the group's `input_boolean`s; any one turning on turns the rest off (`mode: restart`). One automation per tab-group instead of one per tab. Instances: "UI - Kitchen Tabs" (`ui_kitchen_main/dimmers/info`), "UI - Bedroom Tabs" (`ui_bedroom_main/ensuite/aux`) — these replaced the six per-tab automations on 2026-06-12. **Deliberate**: all-booleans-off is a valid state (used as an implicit extra tab on the kiosk dashboards) — do not add a default-tab re-assert.

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

## Remote-Mouse Demo Switch (YAML-only, no custom component)

`switch.remote_mouse_demo` toggles demo mode on the remote-mouse ESP32 (`../remote-mouse` firmware, device `redacted-device` at `REDACTED-IP`). All YAML in `deploy/config/ha/configuration.yaml`:

- `rest:` sensor `sensor.remote_mouse_mode` — polls `http://REDACTED-IP/stats.json` every 15 s; state = `mode`, attributes incl. `engine`/`host_active`/`fw`/`rssi`.
- `rest_command.remote_mouse_set_mode` — `POST /mode?set={{ mode }}`.
- `input_text.remote_mouse_prev_mode` — stashes which autonomous mode was active when switched off.
- Template switch `switch.remote_mouse_demo` — on = an autonomous mode (`demo` or `auto`); off parks the mouse in `move` (the firmware's quiet mode — there is no literal "off"). **Turn-off stashes the active autonomous mode; turn-on restores it** (fallback `auto` if the stash is missing/unknown). Unavailable when the poll sensor is (device offline). Both toggle paths force a sensor refresh ~1 s after the command, so the UI usually confirms within a few seconds rather than the 15 s poll.

Automation "Remote Mouse Schedule" (`remote_mouse_schedule`, server-side) turns the switch on at 09:00 and off at 19:00 on weekdays; no retry if the device is offline at the trigger time.

Gotchas: the device IP is pinned in the router (reservation for `REDACTED-MAC`). The firmware's hardware demo switch (`GET /demo` → `switch_enabled`) must stay disabled, else GPIO1 is authoritative over the mode and fights HA. Adding the first top-level `rest:` section required a full HA restart (`reload_all` only reloads already-loaded integrations).

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

- WebSocket integrations (`shq_display`, `dosa`, `argus`) share a coordinator pattern with:
  - Persistent WebSocket connection with keepalive
  - Real-time state broadcasts from the server
  - Reconnection with backoff on disconnect
  - Availability tracking (30s timeout)
- YAML-configured integrations use dictionary keys as device IDs
- All deps declared in `manifest.json` per component
- HA deploys to `redacted.host` via `./setup ha`
