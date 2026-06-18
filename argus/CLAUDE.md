# Argus

AI-powered alarm assessment daemon. When `alarm_control_panel.shq_alarm`
transitions to `triggered`, Argus captures camera stills, sends them to Claude
with the private premises seed, and logs a natural-language assessment.

**Native x86_64** service that runs on **atlas** (the HA + RAG host) — `cargo
build --release` + systemd, **not** a `cross`/Podman RPi build like nyx/overwatch/dosa.

> **Phase status:** Phase 1 (foundation) complete. The full multi-camera loop,
> tiered Sonnet/Opus, the structured `CaseState`, offsite resilience, the
> outputs (Overwatch/PagerDuty), the kiosk HUD, and the HA component are later
> phases. See `specs/argus/` (read `00-master.md` first).

## Source layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point + CLI. Loads config/seed; `--once` path; daemon loop (select over Ctrl-C and the HA event channel) |
| `src/config.rs` | `config.yaml` loader. `${VAR}` env expansion, `~` expansion, default path `~/.config/argus/config.yaml` |
| `src/version.rs` | `ARGUS_VERSION` from `CARGO_PKG_VERSION` |
| `src/state.rs` | `AlarmState` (Disarmed/Triggered) + `AlarmTracker` (fires only on the edge into Triggered) |
| `src/ha/mod.rs` | HA client module; `websocket_url()` (http→ws) |
| `src/ha/ws.rs` | HA WS client: auth handshake, `subscribe_events`/`state_changed`, reconnect+backoff, emits `HaEvent::AlarmTriggered` over an mpsc channel |
| `src/ha/rest.rs` | `RestClient::snapshot(entity)` → JPEG bytes via `/api/camera_proxy/<entity>` |
| `src/llm/mod.rs` | `AnthropicClient::assess(seed, label, jpeg)` → `Assessment { text, usage }`. Raw HTTP to `/v1/messages`, base64 vision, `cache_control` seed |

## Config

`~/.config/argus/config.yaml` (override `--config`). See `config.yaml.example`.
Secrets are **not** in the file — `${HA_TOKEN}` / `${ANTHROPIC_API_KEY}` are
expanded from the environment at load (systemd `EnvironmentFile`, or the shell).
A missing referenced env var is a hard error at startup.

The **premises seed** (`seed_path`) is the quality ceiling and is private — the
real one lives in `shq-suite-config` / wiki `estate/`. The repo ships only
`seed.example.md`. `config.yaml`, `seed.md`, and `argus.env` are gitignored.

## Anthropic call (Phase 1)

One `claude-sonnet-4-6` `/v1/messages` call per still:

- Headers: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`.
- `system` is an array with the seed in a `cache_control: {type:"ephemeral"}` block — stable content first, so the cache prefix holds. Caches are model-scoped, 5-min TTL.
- `messages[0].content` = a base64 `image/jpeg` block + a short text instruction.
- `max_tokens: 1024`. **No** `temperature`/`top_p`/`thinking` (Phase 1 keeps it plain; those `400` on 4.8 anyway — relevant for the Opus path in Phase 2).
- Returns the concatenated `text` blocks plus the `usage` block. `cache_creation_input_tokens` on the first call within a TTL, `cache_read_input_tokens` on the next — proves the seed cache works.

## Build & run

```bash
cargo build --release                         # native x86_64 (on/for atlas)
./target/release/argus --once                 # assess the first configured camera now
./target/release/argus --once --camera camera.garage_camera_high_resolution_channel
./target/release/argus                         # daemon: watch the alarm
RUST_LOG=argus=debug ./target/release/argus    # verbose (per-state_changed logging)
```

`--once` skips the alarm wait — tests the HA-REST + Anthropic path without arming
the house. Deploy as a systemd **user** service (`argus.service.example`).

## Gotchas

- **HA WS auth order**: server sends `auth_required` → client sends `{type:"auth",access_token}` → expect `auth_ok`. We `subscribe_events` to `state_changed` and filter by `entity_id`, then fire only on the **transition** into `triggered` (the `AlarmTracker` dedupes triggered→triggered attribute-only changes).
- **Reconnect**: backoff resets to 1 s after a successfully-authenticated session, so a transient drop reconnects fast while a down HA backs off (max 30 s).
- **camera_proxy** returns the JPEG in the response body (no temp file) — prefer it over `camera/snapshot`.
- **No Rust Anthropic SDK** → raw `reqwest` HTTP. This is deliberate (see ledger `shq-suite-0002`).
- **Daemon vs `--once`**: the daemon assesses **every** configured camera on each trigger; the shipped example configures one (so "exactly one assessment per trigger" holds). Multi-camera fan-out is formalised in Phase 2.

## Versioning

Bump `Cargo.toml` `version` on every deployed change (logged at startup as
`Starting Argus vX.Y.Z`). MAJOR/MINOR/PATCH per the root `CLAUDE.md` policy.
