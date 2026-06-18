# Phase 1 — Foundation

> Sub-spec of [Argus master plan](./00-master.md).
>
> **Status: ✅ IMPLEMENTED & VERIFIED (2026-06-18).** Crate at `argus/`, builds
> native release warning-free. DoD 1–3 verified live against the real HA + a real
> Anthropic key: native build ✓; a coherent Sonnet assessment of the live garage
> still ✓; the seed prompt-cache engages (`cache_write` then full `cache_read`) ✓.
> Only DoD 4 (a live alarm trigger) is unrun — won't trip the real house alarm
> (the WS path is verified up to subscribe/filter). See Deviations for the
> cache-floor finding.
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

Built largely as specified. Notes for Phase 2:

- **Config is YAML, loaded synchronously at startup** (`serde_yaml`), parsed
  *after* a manual `${VAR}` env-expansion pass over the raw text. Only the
  `${VAR}` form is expanded (not bare `$VAR`); a referenced-but-unset var is a
  **hard startup error** (so a missing secret fails loudly, never sends an empty
  token). `~/` in `seed_path` is expanded to `$HOME`. Default config path is
  `~/.config/argus/config.yaml` via `directories::ProjectDirs("","","argus")`.
- **Trigger mechanism = `subscribe_events` on `state_changed`, filtered by
  `entity_id`** (not `subscribe_trigger`). Reason: simpler, and `state_changed`
  carries `new_state.state` directly. The `AlarmTracker` (in `state.rs`) fires
  only on the **edge into `triggered`** — `AlarmState` collapses every non-
  `triggered` HA state to `Disarmed`, so triggered→triggered attribute-only
  events don't re-fire. Subscribe id is `1` (single subscription per connection).
- **HA payload shapes** observed/handled: `{"type":"auth_required"}` →
  `{"type":"auth","access_token":…}` → `{"type":"auth_ok"}` (or `auth_invalid`);
  events arrive as `{"type":"event","event":{"event_type":"state_changed",
  "data":{"entity_id":…,"new_state":{"state":…}}}}`. The `subscribe_events`
  ack (`{"type":"result","success":true}`) is ignored (type != `event`).
- **WS reconnect**: exponential backoff 1 s → 30 s, **reset to 1 s after a
  successfully-authenticated session**, so a transient drop reconnects fast while
  a down HA backs off. Ping frames are answered with Pong in the main loop.
- **Anthropic call** is exactly the spec shape (raw `reqwest`, `x-api-key` +
  `anthropic-version: 2023-06-01`, base64 `image/jpeg`, seed in a
  `cache_control:{type:"ephemeral"}` system block, `max_tokens:1024`, no
  sampling/thinking params). Non-2xx surfaces the response body in the error; a
  `stop_reason:"refusal"` is logged (the assessment may then be empty).
- **Daemon assesses *every* configured camera** on each trigger (the shipped
  example configures one, so "exactly one assessment per trigger" holds). The
  decoupling is an mpsc channel: `ha::ws::run` emits `HaEvent::AlarmTriggered`;
  `main`'s `select!` loop consumes it (and Ctrl-C).
- **What is verified vs pending** (verified live 2026-06-18 with a real HA + real
  Anthropic key): native release build ✓ (warning-free); HA `camera_proxy`
  snapshot returns a real JPEG (garage, ~17.7 KB) ✓; WS connect/auth/subscribe
  against live HA (`atlas.shq.sh:8123`) ✓; **DoD 2** — a coherent Sonnet 4.6
  assessment of the live garage view (correctly reported no person, identified
  the vehicle + the probable trigger, flagged template-seed mode) ✓; **DoD 3** —
  the seed prompt-cache engages: run 1 `cache_write=3308`, run 2 `cache_read=3308`
  (full hit) ✓. **Only DoD 4 pending** — a live alarm trigger producing exactly
  one assessment + reconnect-survives-HA-restart (not run: won't trip the real
  house alarm; safe alternative is to point `alarm_entity` at a scratch
  `input_select`/`input_text` and set it to `triggered`).
- **CACHE FLOOR — important for the real seed.** Sonnet 4.6's minimum cacheable
  prefix is ~2048 tokens. The shipped `seed.example.md` is ~2036 **bytes**
  (~500 tokens), **below the floor**, so with the template seed the
  `cache_control` block is silently a no-op (`cache_write=0/cache_read=0`) — this
  is expected, not a bug. Verified by padding the seed to ~12 KB (3308 tokens):
  the cache then writes and reads correctly. **The real premises seed must exceed
  ~2048 tokens for the master spec's ~90%-off cache-read economics to hold** —
  it will, easily (floor plan + camera map + whitelist + escalation policy), but
  keep it in mind when authoring, and don't be alarmed by zero cache stats while
  testing against the small template.
- **`config.AnthropicConfig.id_model`** is parsed but not yet read
  (`#[allow(dead_code)]`) — reserved for the Phase 2 Opus path.

## Inputs to Phase 2

Concrete API surface Phase 2 inherits (all in `argus/src/`):

- **Config entry points** (`config.rs`): `Config::load(Option<PathBuf>) ->
  Result<Config>` and `Config::load_seed(&self) -> Result<String>`. `Config`
  fields: `ha {url, token}`, `anthropic {api_key, live_model, id_model}`,
  `seed_path`, `cameras: Vec<CameraConfig{entity, label}>`, `alarm_entity`.
- **HA REST** (`ha::rest`): `RestClient::new(base_url, token) -> Result<Self>`;
  `async RestClient::snapshot(&self, entity: &str) -> Result<Vec<u8>>` (JPEG
  bytes). Phase 2 multi-camera capture fans this out across `cfg.cameras`.
- **HA WS** (`ha::ws`): `async run(ws_url, token, alarm_entity, tx:
  mpsc::Sender<HaEvent>)` (loops forever). `HaEvent::AlarmTriggered{entity_id}`.
  `ha::websocket_url(base)` does the http→ws conversion. Phase 2 extends this to
  also pull **sensor telemetry** (another `subscribe_events` filter or a REST
  state read) and to track richer alarm states.
- **Anthropic** (`llm`): `AnthropicClient::new(api_key, model) ->
  Result<Self>`; `async assess(&self, seed: &str, camera_label: &str, jpeg:
  &[u8]) -> Result<Assessment>`. `Assessment { text: String, usage: Usage }`;
  `Usage { input_tokens, output_tokens, cache_creation_input_tokens,
  cache_read_input_tokens }`. **Phase 2 needs**: an Opus client instance (second
  `AnthropicClient` on `id_model`), structured output (swap the plain-text
  parse for `output_config.format` / a `CaseState` schema), and the prompt-cache
  warming loop. The request builder lives in `assess()` — generalise it to take
  the model, multiple images, telemetry text, and the structured-output config.
- **State** (`state.rs`): `AlarmState`, `AlarmTracker` — grows into the full
  state machine + `CaseState` machine in Phase 2.
- **Seed format that worked**: plain Markdown, sent verbatim as the cached system
  text. `seed.example.md` is the public template; the real seed is private.
