# Actron SHQ

Home Assistant integration for Actron air conditioning systems via the `actron-neo-api` cloud SDK. Uses a fault-tolerant API wrapper ported from `actron-poc/`.

## Entities

| Entity | Type | Description |
|--------|------|-------------|
| `climate.actron_air_conditioner` | Climate | Main unit — HVAC mode, fan mode, target temp |
| `climate.actron_{zone_name}` | Climate | Per-zone — on/off, target temp (inherits parent mode) |
| `sensor.actron_outdoor_temperature` | Sensor | Outdoor temperature (°C) |
| `sensor.actron_humidity` | Sensor | Humidity (%) |
| `sensor.actron_controller_state` | Sensor (diagnostic) | Coordinator state: `idle`/`pending`/`timeout`/`rate_limited` |
| `switch.actron_continuous_fan` | Switch | Continuous fan mode |
| `switch.actron_away_mode` | Switch | Away mode |
| `switch.actron_quiet_mode` | Switch | Quiet mode |
| `switch.actron_turbo_mode` | Switch | Turbo mode |

## Config

UI config flow using OAuth2 device-code authentication. Stores only `refresh_token` in config entry data.

## Architecture

- **Config flow** (`config_flow.py`): 2-step device-code OAuth2. Step 1 requests code and shows URL + code. Step 2 polls for token completion.
- **API wrapper** (`api.py`): Fault-tolerant wrapper around `ActronAirAPI`. Every SDK call goes through `_call()` with exponential backoff (2s base, 30s max), 60s timeout, 3 max retries, auth-error token refresh. Detects 429/503 responses (via `Status:` prefix in the SDK's error message) and raises `ActronRateLimitError` without retrying.
- **Coordinator** (`coordinator.py`): `DataUpdateCoordinator` with 30s baseline polling. Owns a keyed **optimistic overlay** (field name → value) and a **burst window**: after any command, polling accelerates to 1s for 15s; further commands reset the window. On each poll, overlay entries whose underlying field changed are cleared (server authoritative, handles external changes too). On burst expiry, any un-confirmed overlays are recorded as `timed_out_keys`. On rate-limit errors, burst is aborted and polling backs off for 60s. `controller_state` / `controller_state_attributes` expose this for the diagnostic sensor.
- **Climate** (`climate.py`): Main + zone entities. Most commands go through `_run_command()` which sets the coordinator overlay, acquires `command_lock`, and runs the API call. Per-slot cancellation (e.g. "temperature", "mode") discards in-flight predecessors so rapid slider drags only send the final value. Optimistic state lives on the coordinator, not the entity, so concurrent edits across entities don't clobber each other.
- **Zone on/off** (`climate.py` → `_toggle_zone`): special-cased, the cloud is slow/lossy here. Instead of firing once, it **re-sends** the enable command every `ZONE_RESEND_INTERVAL_SECONDS` until the overlay clears (controller confirms) or `ZONE_CONFIRM_TIMEOUT_SECONDS` elapses. Before turning off what is currently the *last* active zone, it **holds** the API call (outside `command_lock`, so a concurrent zone turn-on can still get through) and waits up to `LAST_ZONE_OFF_HOLD_SECONDS` for another zone to become active *on the controller* — pending overlays are excluded so an in-flight HA turn-on doesn't count. If none activates it **refuses** (reverts to on, raises `HomeAssistantError`). Confirmation/hold waits call `coordinator.extend_burst()` each tick to keep 1s polling alive and avoid premature overlay timeout.
- **Sensors** (`sensor.py`): Outdoor temperature, humidity, and controller state (diagnostic).
- **Switches** (`switch.py`): Feature toggles (continuous fan, away, quiet, turbo). Data-driven via `ActronSwitchConfig` — each config maps a read property, overlay key, and API method. Uses the same coordinator overlay.

## HVAC Mode Mapping

| HA Mode | SDK Mode |
|---------|----------|
| OFF | OFF |
| COOL | COOL |
| HEAT | HEAT |
| AUTO | AUTO |
| FAN_ONLY | FAN |

## Gotchas

- **Main unit has no `turn_off`/`turn_on`**: `ActronClimate` (the main unit) only declares `TARGET_TEMPERATURE | FAN_MODE`, so `climate.turn_off`/`climate.turn_on` raise a 500. Turn it off with `climate.set_hvac_mode` → `off` instead. (Zone entities *do* support `turn_on`/`turn_off`.)
- **Turning off the last active zone shuts down the whole system** and latches it off until a zone is re-enabled. `_toggle_zone` refuses this (see Zone on/off above) — to actually stop the system, set the main unit's HVAC mode to `off`.
- **Zone enable sends the whole `EnabledZones` array** (SDK `set_enable_command`), not a single-zone delta. That's why `enabled_zones` is mutated locally before sending and re-asserted on each re-send — so concurrent zone commands and re-sends build a correct full array against the latest server state.

## Key Files

| File | Purpose |
|------|---------|
| `const.py` | Domain, poll intervals (base 30s, burst 1s for 15s, rate-limit cooldown 60s), zone retry (resend 5s / timeout 90s) and last-zone-off hold (60s) |
| `manifest.json` | Dependencies (`actron-neo-api`), config_flow, cloud_polling |
| `config_flow.py` | Device-code OAuth2 (2-step) |
| `api.py` | Fault-tolerant SDK wrapper (backoff, retry, auth refresh) |
| `coordinator.py` | DataUpdateCoordinator — 30s poll with 1s×15s burst after commands, optimistic overlay, rate-limit backoff |
| `climate.py` | Main + zone climate entities |
| `sensor.py` | Outdoor temp + humidity sensors |
| `switch.py` | Feature toggles (continuous fan, away, quiet, turbo) |

## SDK Classes Used

- `ActronAirAPI` — main SDK class, `refresh_token` param, `request_device_code()`, `poll_for_token()`
- `ActronAirAuthError`, `ActronAirAPIError` — exception types
- `status.user_aircon_settings` — `is_on`, `mode`, `fan_mode`, `temperature_setpoint_cool_c`, `temperature_setpoint_heat_c`, `continuous_fan_enabled`, `away_mode`, `quiet_mode_enabled`, `turbo_enabled`
- `status.remote_zone_info[i]` — `is_active`, `title`, `live_temp_c`, `temperature_setpoint_cool_c/heat_c`
- `status.outdoor_temperature`, `status.humidity`
