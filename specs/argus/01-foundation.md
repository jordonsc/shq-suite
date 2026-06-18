# Phase 1 — Foundation

> Sub-spec of [Argus master plan](./00-master.md).
>
> **Status: 📝 NOT STARTED.**
>
> **Goal:** prove the brain end-to-end in the smallest slice — when the alarm transitions to
> `triggered`, Argus captures **one** camera still, sends it to Claude with the premises seed, and
> logs a coherent assessment. No loop, no outputs, no kiosk yet.

## Context

Phase 1 lays the Argus skeleton on `atlas`: the crate, config, the Home Assistant client (watch the
alarm state, pull a camera still), and the Anthropic vision client. It de-risks the two unknowns —
HA stills and the Claude vision call working on `atlas` — before any of the orchestration or theatre
is built. By the end, arming-then-triggering the alarm produces a logged natural-language assessment
of what one camera sees.

## Scope

**In scope:**
- `argus/` crate scaffold (native x86_64): `Cargo.toml`, `src/main.rs`, `src/config.rs`,
  `src/version.rs`, `src/ha/mod.rs` (WS + REST), `src/llm/mod.rs` (Anthropic), `src/state.rs`.
- `argus/config.yaml.example` (placeholders) + `argus/seed.example.md` (the premises-seed template).
- HA **WS client**: connect, authenticate, subscribe to the alarm entity, detect `→ triggered`.
- HA **REST client**: pull a JPEG still for one configured camera via `/api/camera_proxy/<entity>`.
- Anthropic client: one `claude-sonnet-4-6` `/v1/messages` call — seed (cached) + one image → text.
- `argus/CLAUDE.md` (initial), `argus/README.md`, a systemd unit example.
- A `--once`/dry-run mode and a way to simulate the trigger for testing without arming the house.

**Out of scope (later phases):**
- The continuous loop, multi-camera capture, sensor telemetry, tiered Opus calls — Phase 2.
- Structured `CaseState` — Phase 2 (Phase 1 returns plain text).
- Overwatch voice, PagerDuty — Phase 3.
- Kiosk web app / takeover — Phase 4.
- HA `argus` component, the deploy-tool target — Phase 5 (Phase 1 builds + runs manually on atlas).

## Implementation

### 1. Crate scaffold
`argus/Cargo.toml` — native build (no `Cross.toml`, no `build-rpi.sh`). Deps: `tokio` (full),
`tracing` + `tracing-subscriber`, `serde`/`serde_json`/`serde_yaml`, `reqwest` (json, rustls),
`tokio-tungstenite`, `anyhow`, `base64`, `clap` (CLI). `src/version.rs`: a `const ARGUS_VERSION`
sourced from `env!("CARGO_PKG_VERSION")`, logged at startup.

### 2. Config (`src/config.rs`, `config.yaml.example`)
```yaml
ha:
  url: "http://localhost:8123"        # atlas-local
  token: "${HA_TOKEN}"                # long-lived token; env-expanded, never committed
anthropic:
  api_key: "${ANTHROPIC_API_KEY}"
  live_model: "claude-sonnet-4-6"     # Phase 1 uses this one
  id_model: "claude-opus-4-8"         # reserved for Phase 2
seed_path: "~/.config/argus/seed.md"  # the private premises seed (Phase 1 may use seed.example.md)
cameras:
  - entity: "camera.garage_camera_high_resolution_channel"
    label: "Garage"                   # human label included in the prompt
alarm_entity: "alarm_control_panel.shq_alarm"
```
Resolve `${VAR}` from the environment so secrets stay out of the file. Persistent location
`~/.config/argus/config.yaml`.

### 3. HA WS client (`src/ha/ws.rs`)
- Connect `ws://<host>:8123/api/websocket`. Server sends `{type:"auth_required"}` → reply
  `{type:"auth", access_token: <token>}` → expect `{type:"auth_ok"}`.
- Subscribe to the alarm entity: `{id, type:"subscribe_trigger", trigger:{platform:"state",
  entity_id:"alarm_control_panel.shq_alarm"}}` (or `subscribe_events`/`state_changed` + filter).
- On an event where `to_state.state == "triggered"`, fire the Phase-1 handler. Track the prior state
  so only the **transition** fires (not repeats). Auto-reconnect with backoff (mirror the
  coordinator pattern in `home-assistant/custom_components/shq_display/coordinator.py`).

### 4. HA REST client (`src/ha/rest.rs`)
- `GET <url>/api/camera_proxy/<entity_id>` with `Authorization: Bearer <token>` → JPEG bytes.
  (Alternative: POST `camera/snapshot` to a file — prefer `camera_proxy`, it returns bytes in-memory.)
- Helper `snapshot(entity) -> Vec<u8>` returning the JPEG.

### 5. Anthropic client (`src/llm/mod.rs`)
- `POST https://api.anthropic.com/v1/messages`, headers `x-api-key`, `anthropic-version: 2023-06-01`,
  `content-type: application/json`.
- Body: `model: live_model`, `max_tokens: 1024`,
  `system: [{type:"text", text: <seed>, cache_control:{type:"ephemeral"}}]`,
  `messages: [{role:"user", content: [ {type:"image", source:{type:"base64", media_type:"image/jpeg",
  data:<b64>}}, {type:"text", text:"Camera: Garage. The intruder alarm has triggered. Describe what
  you see — who is present, what they are doing, and where."} ]}]`.
- Parse `content[].type == "text"` → the assessment string. Log `usage` (confirm
  `cache_creation_input_tokens` on first call, `cache_read_input_tokens` on a second — proves the
  seed cache works).

### 6. Wire-up (`src/main.rs`, `src/state.rs`)
- Load config + seed, init tracing, connect HA WS.
- On `→ triggered`: snapshot the configured camera → Claude → `info!` the assessment.
- `src/state.rs`: a minimal `AlarmState { Disarmed, Triggered }` enum + last-seen tracking (grows
  into the full machine in Phase 2).

### 7. Test affordances
- `argus --once --camera <entity>`: skip the HA WS wait, snapshot + assess immediately (tests the
  HA-REST + Anthropic path without arming the house).
- A documented way to simulate the trigger: set the alarm via the API
  (`./ha post /api/services/alarm_control_panel/alarm_trigger '{"entity_id":"alarm_control_panel.shq_alarm"}'`)
  or, safer, point `alarm_entity` at a scratch `input_select`/helper during dev.

## Config / secrets
`ANTHROPIC_API_KEY` + `HA_TOKEN` from the environment (systemd `EnvironmentFile` or the secret
store). `config.yaml.example` ships placeholders; the real `config.yaml` is gitignored and mirrored
to `shq-suite-config`. A long-lived HA token is minted in the HA UI (profile → long-lived tokens).

## Verification (definition of done)
1. `cargo build --release` on atlas (native, no cross).
2. `argus --once --camera camera.garage_camera_high_resolution_channel` logs a sensible assessment
   of the current garage view.
3. A second `--once` run shows `cache_read_input_tokens > 0` (seed cache hit).
4. Running as a daemon, triggering `alarm_control_panel.shq_alarm` produces exactly one assessment
   per trigger transition; reconnect survives an HA restart.

## References
- `home-assistant/CLAUDE.md` (HA API access, the `./ha` helper, entity-id patterns).
- `home-assistant/custom_components/shq_display/coordinator.py` (WS connect/auth/reconnect pattern).
- `nyx/CLAUDE.md` (config-at-`~/.config` precedent).
- claude-api skill: raw-HTTP vision + prompt-caching shapes (cited in the master).

---

## Deviations from spec
_(Implementing agent: record here how the built code diverged — endpoint quirks, the chosen trigger
mechanism, HA payload shapes, anything Phase 2 must know. Authoritative over the master where they
conflict.)_

## Inputs to Phase 2
_(Implementing agent: list what Phase 2 inherits — the HA client API surface, the camera-snapshot
helper signature, the Anthropic client signature, the seed format that actually worked, the
config-loading entry points.)_
