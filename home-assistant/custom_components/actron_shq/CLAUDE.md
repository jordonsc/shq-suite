# Actron SHQ

Home Assistant integration for Actron air conditioning systems via the `actron-neo-api` cloud SDK. Uses a fault-tolerant API wrapper ported from `actron-poc/`.

## Entities

| Entity | Type | Description |
|--------|------|-------------|
| `climate.actron_air_conditioner` | Climate | Main unit — HVAC mode, fan mode, target temp |
| `climate.actron_{zone_name}` | Climate | Per-zone — on/off, target temp (inherits parent mode) |
| `sensor.actron_outdoor_temperature` | Sensor | Outdoor temperature (°C) |
| `sensor.actron_humidity` | Sensor | Humidity (%) |
| `switch.actron_continuous_fan` | Switch | Continuous fan mode |
| `switch.actron_away_mode` | Switch | Away mode |
| `switch.actron_quiet_mode` | Switch | Quiet mode |
| `switch.actron_turbo_mode` | Switch | Turbo mode |

## Config

UI config flow using OAuth2 device-code authentication. Stores only `refresh_token` in config entry data.

## Architecture

- **Config flow** (`config_flow.py`): 2-step device-code OAuth2. Step 1 requests code and shows URL + code. Step 2 polls for token completion.
- **API wrapper** (`api.py`): Fault-tolerant wrapper around `ActronAirAPI`. Every SDK call goes through `_call()` with exponential backoff (2s base, 30s max), 60s timeout, 3 max retries, auth-error token refresh.
- **Coordinator** (`coordinator.py`): `DataUpdateCoordinator` polling every 60s. Discovers system serial on setup, returns `ActronAirStatus` Pydantic object as data.
- **Climate** (`climate.py`): Main entity + one per zone. Main has HVAC + fan modes. Zones have on/off + target temp only. All commands go through `_execute_command()` which cancels any in-flight command for the same slot (e.g. "temperature", "mode", "fan_mode") — rapid UI adjustments only send the final value.
- **Sensors** (`sensor.py`): Outdoor temperature and humidity from coordinator data.
- **Switches** (`switch.py`): Feature toggles (continuous fan, away, quiet, turbo). Data-driven via `ActronSwitchConfig` — each config maps a read property and API method name.

## HVAC Mode Mapping

| HA Mode | SDK Mode |
|---------|----------|
| OFF | OFF |
| COOL | COOL |
| HEAT | HEAT |
| AUTO | AUTO |
| FAN_ONLY | FAN |

## Key Files

| File | Purpose |
|------|---------|
| `const.py` | Domain, poll interval |
| `manifest.json` | Dependencies (`actron-neo-api`), config_flow, cloud_polling |
| `config_flow.py` | Device-code OAuth2 (2-step) |
| `api.py` | Fault-tolerant SDK wrapper (backoff, retry, auth refresh) |
| `coordinator.py` | DataUpdateCoordinator — 60s poll, system discovery |
| `climate.py` | Main + zone climate entities |
| `sensor.py` | Outdoor temp + humidity sensors |
| `switch.py` | Feature toggles (continuous fan, away, quiet, turbo) |

## SDK Classes Used

- `ActronAirAPI` — main SDK class, `refresh_token` param, `request_device_code()`, `poll_for_token()`
- `ActronAirAuthError`, `ActronAirAPIError` — exception types
- `status.user_aircon_settings` — `is_on`, `mode`, `fan_mode`, `temperature_setpoint_cool_c`, `temperature_setpoint_heat_c`, `continuous_fan_enabled`, `away_mode`, `quiet_mode_enabled`, `turbo_enabled`
- `status.remote_zone_info[i]` — `is_active`, `title`, `live_temp_c`, `temperature_setpoint_cool_c/heat_c`
- `status.outdoor_temperature`, `status.humidity`
