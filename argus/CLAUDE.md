# Argus

AI-powered alarm assessment daemon. When `alarm_control_panel.shq_alarm`
transitions to `triggered`, Argus captures camera stills, sends them to Claude
with the private premises seed, and logs a natural-language assessment.

**Native x86_64** service that runs on **atlas** (the HA + RAG host) — `cargo
build --release` + systemd, **not** a `cross`/Podman RPi build like nyx/overwatch/dosa.

> **Phase status:** Phases 1 (foundation), 2 (assessment loop + `CaseState`), 2a
> (offsite S3 resilience), 3 (outputs — Overwatch voice + PagerDuty), and 4
> (kiosk HUD server + takeover — Rust side) are implemented + shape-verified.
> **Phase 5 (control WS — Rust side)** is implemented: the `/control` WebSocket on
> the HUD server + the `ControlCommand` channel into the engine (the HA `argus`
> component connects here). The HA component itself + deploy + the real seed are
> owned by other Phase 5 work. See `specs/argus/` (read
> `00-master.md` first). **Phase 3 is NOT live-fired**: voice
> (`SetAlarm`/`Verbalise`) and PagerDuty sends are wired + compiled + shape-tested
> only — no real klaxon/page has been sent (residence-asleep constraint).
> **Phase 4 takeover is NOT live-fired**: the HUD server is built + locally
> smoke-tested (HTTP routes + `/kiosk` WS handshake against the real `web/` app),
> but no real kiosk has been navigated (residence-asleep). The `web/` HUD app is
> owned by the frontend; the Rust side serves it.

## The `CaseState` contract

`src/case.rs` defines **`CaseState`** — the single source of truth for an alarm
episode, and the central interface of the whole project: the engine produces it;
Phases 2a/3/4 consume it (none of them call the LLM or HA). It is serialised to
the `watch` broadcast channel, journalled to the case dir, and (Phase 2a)
replicated to S3 — **one schema, three surfaces**. The LLM returns *deltas*
(`LiveAssessment` from Sonnet, `Identification` from Opus, via
`output_config.format` json_schema); Argus reconciles them and **derives the
`timeline`** (closed `TimelineKind` vocabulary) by diffing — Phase 3 maps those.

## Source layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point + CLI. Loads config/seed; builds the Sonnet + Opus clients + the `watch<Option<CaseState>>` channel; `--once` (single tick → print CaseState) and the daemon (Ctrl-C aborts the engine task) |
| `src/config.rs` | `config.yaml` loader. `${VAR}` env expansion, `~` expansion, default path `~/.config/argus/config.yaml`. Adds `loop_config` (cadence/cap) + `telemetry_entities` + `offsite` (Phase 2a S3 replication; `OffsiteConfig`/`UploadConfig`) |
| `src/case.rs` | **The `CaseState` contract** + `LiveAssessment`/`Identification` LLM-output types, their `*_schema()` JSON schemas, `TimelineKind` vocabulary, and `CaseDir` (the on-disk journal = Phase 2a upload queue) |
| `src/engine.rs` | The assessment loop: per-tick multi-camera capture + telemetry → Sonnet live loop → merge into `CaseState` → best stills + Opus forensic; state machine; daily token cap; broadcast + journal. **Phase 5**: defines `ControlCommand::{Standdown, Acknowledge}`; `run()` `select!`s a `mpsc::Receiver<ControlCommand>` alongside the HA event stream + tick — `Standdown` reuses the `standdown()` path, `Acknowledge` pushes a `TimelineKind::Acknowledged` milestone + broadcasts (both no-op + log when no case is active) |
| `src/version.rs` | `ARGUS_VERSION` from `CARGO_PKG_VERSION` |
| `src/state.rs` | `AlarmState` (Disarmed/Triggered) + `AlarmTracker` → `Transition::{IntoTriggered, OutOfTriggered}` (both edges) |
| `src/ha/mod.rs` | HA client module; `websocket_url()` (http→ws) |
| `src/ha/ws.rs` | HA WS client: auth handshake, `subscribe_events`/`state_changed`, reconnect+backoff, emits `HaEvent::{AlarmTriggered, AlarmCleared}` over an mpsc channel |
| `src/ha/rest.rs` | `RestClient::snapshot(entity)` → JPEG; `state(entity)` + `telemetry(entities)` → per-tick sensor text |
| `src/llm/mod.rs` | `AnthropicClient::assess(&AssessRequest)` → `Completion { text, usage }`. Raw HTTP to `/v1/messages`; multi-image base64 vision, `cache_control` seed, `Reasoning::{Fast,Deep}` (Sonnet effort-low/no-think vs Opus adaptive+high), optional `output_config.format` structured output |
| `src/out/mod.rs` | Output-sink module (Phase 2a+). Declares `offsite`/`voice`/`voice_policy`/`pagerduty`; sinks consume the `CaseState` stream + case dir, none call the LLM/HA. **Phase 3 `run()`** subscribes to the `watch` channel, diffs the timeline per `case_id` (length cursor), and routes each NEW `TimelineEvent` to the voice gate + PagerDuty as INDEPENDENT channels (one failing never blocks the other). Speech is serialised through a single voice worker (mpsc) so milestones don't talk over each other; klaxon on/off is tunnelled as a sentinel on the same channel so a stop can't race a line |
| `src/out/voice.rs` | **Phase 3** Overwatch gRPC client. `build.rs` (tonic-build 0.11) compiles `proto/voice.proto` (a relative symlink to `../overwatch/proto/voice.proto`) into the `voice` module (client only). `VoiceClient::{set_alarm,verbalise}` dial `host:port` lazily per call; on connect/RPC failure they log + return (never panic, never block the PD channel) |
| `src/out/voice_policy.rs` | **Phase 3** the POSITIVE-ONLY gate: pure `line_for(event, state) -> Option<SpokenLine>`. A whitelist `match` over `TimelineKind` — `case_opened`/`intruder_identified`/`best_still_upgraded`/`security_station_notified` speak; `threat_level_changed` speaks ONLY on escalation (parses the engine's `"<from> → <to>"` detail); `intruder_detected`/`standdown`/`cleared` → `None`. Structurally cannot emit a failure line (failures aren't timeline events). Unit-tested (no network) |
| `src/out/pagerduty.rs` | **Phase 3** PagerDuty Events v2 (`reqwest`, reuses the rustls client). `build_event()` (PURE, no network — the shape-test surface) is split from `send()`. `trigger` (dedup_key=`case_id`) on case open + material change; `resolve` on standdown. `custom_details` = threat level + intruders + recent timeline + the deterministic `s3://<bucket>/<prefix>/<case_id>/` prefix when offsite is enabled (bare prefix — atlas has write-only creds, NO presigned URL). Routing key never logged |
| `src/web/mod.rs` | **Phase 4 + 5** the kiosk HUD HTTP/WS server (`axum` + tower-http `ServeDir`). `serve(cfg, rx, case_base, control_tx)`: `GET /alarm` (+ `/` fallback) → the static `web/` app; `GET /stills/:id` → the JPEG for still `<id>` from the CURRENT case's `stills/<id>.jpg` (resolved via the latest `CaseState` on the watch channel), falling back to scanning every `cases/*/stills/<id>.jpg`; `GET /kiosk` → a WebSocket that sends the current `CaseState` (or `null`) on connect and pushes the full serde `CaseState` JSON on every change. **`GET /control` (Phase 5)** → the HA `argus` component's WS: pushes a COMPACT status object (not the full `CaseState`) on connect + every change, and forwards inbound `acknowledge`/`standdown` commands to the engine over `control_tx`. Binds `web.bind` (LAN, shared with `/control` — no separate port). Tolerates client disconnects + a missing `index.html` (404s until the frontend lands). Does NOT call the LLM/HA |
| `src/out/kiosks.rs` | **Phase 4** kiosk-takeover consumer: a `watch` consumer that on the first `CaseState` of a case (`triggered`/`assessing`) calls `shq_display.navigate` (`{device_id, url}`) for each configured kiosk → `<web.public_base>/alarm`, and on `cleared` navigates each back to its `dashboard_url`. Per-case deduped (navigate at most once each way). Uses `RestClient::call_service`. Failures logged, never fatal. **Live takeover deferred** (residence asleep) — built + compiled + locally smoke-tested, no real kiosk navigated. **nyx dependency** (see below) |
| `src/out/offsite.rs` | **Phase 2a** offsite S3 replicator: `run(cfg, base, wake)` walks `<base>/cases/`, PUTs each non-`.tmp`/un-`.uploaded` file (events+stills first), drops a `.uploaded` marker, retries with `retry_backoff_secs`. Woken by the `watch` receiver, with a `scan_interval_secs` re-scan fallback (+ startup crash recovery). EXPLICIT static write-only creds (`aws-sdk-s3`, rustls), NOT the ambient chain; relies on bucket-default Object Lock |

## Phase 4 — kiosk HUD (Rust side)

The HUD HTTP/WS server (`src/web/mod.rs`) + the takeover consumer
(`src/out/kiosks.rs`) are spawned in DAEMON mode only: the server iff `web:` is
configured; the takeover consumer iff `web:` is configured AND `kiosks:` is
non-empty. Their `watch::Receiver`s are taken in `main` via `state_tx.subscribe()`
BEFORE `state_tx` moves into the engine (alongside the 2a/3 receivers). The
takeover consumer builds its own `RestClient` (the engine owns the other one).

**Endpoints** (bind `web.bind`, default `0.0.0.0:8770`):

| Endpoint | Serves |
|----------|--------|
| `GET /alarm` (+ `/`) | the static HUD app from `web.dir` (the frontend's `web/`; `index.html` is the SPA) |
| `GET /stills/:id` | `<case_base>/cases/<case_id>/stills/<id>.jpg` as `image/jpeg`; current case first, else a scan of all case dirs; 404 if missing. Accepts `<id>` or `<id>.jpg` |
| `GET /kiosk` | WebSocket — sends the current `CaseState` (or `null`) on connect, then pushes the full serde `CaseState` JSON on every change. Reconnect is the client's job |
| `GET /control` | **Phase 5** WebSocket for the HA `argus` component (same `web.bind` port, NOT a separate listener). See below |

The `/kiosk` WS frame is the SAME serde serialisation as the on-disk journal —
one schema, three surfaces. The frontend references stills by
`intruders[].best_stills[].id` → `/stills/<id>.jpg`.

## Phase 5 — control WS (Rust side)

`GET /control` (on the same `web.bind` HUD server — reuses the Phase-4 axum
router + the `watch<Option<CaseState>>` state; **no second listener/port**). The
HA `argus` component connects here for status + control.

**Server → client** — a COMPACT status object (NOT the full `CaseState`), sent
once on connect and on every `CaseState` change:

```json
{"type":"status","version":"<argus semver>","active":<bool>,"case_id":<string|null>,
 "case_status":<string|null>,"threat_level":<string|null>,"intruder_count":<int>,
 "summary":<string|null>,"updated_at":<string|null>}
```

`active` = a case exists whose status != `cleared`; the projection fields are
null/0 when there is no case. `case_status` (not `status`) carries the case's
status string so it never collides with the message `type`.

**Client → server** — a command: `{"type":"command","command":"acknowledge"}` or
`{"type":"command","command":"standdown"}`. Recognised commands are forwarded to
the engine over the `mpsc::Sender<ControlCommand>` and answered best-effort with
`{"type":"ack","command":<cmd>,"ok":true}`; malformed/unknown frames are logged +
ignored (never panic). `standdown` → the engine's `standdown()` path (same as an
HA `AlarmCleared` edge); `acknowledge` → a `TimelineKind::Acknowledged` milestone
(detail "Operator acknowledged.") + broadcast.

**Wiring:** `main` creates `mpsc::channel::<ControlCommand>()` unconditionally;
the `Receiver` flows into the engine (`run(rx, control_rx)`), the `Sender` is
cloned into `web::serve(...)` only when `web:` is configured (Argus always runs
the web server in production, so the control surface is always present there). No
new config — the control WS shares the `web` port. `--once` does NOT use the
control channel (it calls `run_once`, never `run`).

**nyx dependency (live-takeover blocker, flagged):** a bare
`shq_display.navigate` only does a CDP `Page.navigate`. It does NOT wake a
sleeping kiosk (backlight off) nor kill a Chronos clock overlay (which sits
*above* Chrome on `idle_mode: clock` kiosks), so on those kiosks the takeover is
invisible. nyx's `wake`/`set_display true` is what SIGKILLs Chronos + restores
the backlight (`nyx/CLAUDE.md`, ledger `shq-suite-0001`). Needed before live
takeover: either a small nyx change so `navigate` implies wake+overlay-kill, or a
`shq_display.wake` service the consumer calls first. NOT yet wired (no
`shq_display.wake` exists today) — see `04-kiosk-hud.md` Deviations.

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
./target/release/argus --once                 # ONE full assessment cycle now → print CaseState JSON
./target/release/argus                         # daemon: watch the alarm, run the loop while triggered
RUST_LOG=argus=debug ./target/release/argus    # verbose (per-state_changed logging)
```

`--once` opens an ephemeral case, runs one multi-camera tick, prints the
`CaseState`, and stands down — tests the whole HA + tiered-Anthropic pipeline
without arming the house. Cases are written to `~/.local/share/argus/cases/`.
Deploy as a systemd **user** service (`argus.service.example`).

## Gotchas

- **HA WS auth order**: server sends `auth_required` → client sends `{type:"auth",access_token}` → expect `auth_ok`. We `subscribe_events` to `state_changed` and filter by `entity_id`, then fire only on the **transition** into `triggered` (the `AlarmTracker` dedupes triggered→triggered attribute-only changes).
- **Reconnect**: backoff resets to 1 s after a successfully-authenticated session, so a transient drop reconnects fast while a down HA backs off (max 30 s).
- **camera_proxy** returns the JPEG in the response body (no temp file) — prefer it over `camera/snapshot`.
- **No Rust Anthropic SDK** → raw `reqwest` HTTP. This is deliberate (see ledger `shq-suite-0002`).
- **Prompt-cache floors differ by model**: Sonnet 4.6 caches a ≥2048-token prefix; **Opus 4.8 needs ≥4096 tokens**. `seed.example.md` (~2265 tokens) caches on Sonnet but is below the Opus floor — the **real premises seed must exceed ~4096 tokens** for the Opus forensic path to get cache reads.
- **Structured output**: the LLM returns JSON via `output_config.format` (`{type:"json_schema", schema}`). Schema rules: every object `additionalProperties:false`, all props `required`, optional fields nullable-but-required (`["string","null"]`), **no** numeric/string constraints (`minimum`/`maxLength`). The model emits *deltas* (`LiveAssessment`/`Identification`); Argus derives the `timeline`.
- **Case dir = upload queue**: each `events/NNNNNN.json` + `stills/*.jpg` is written atomically (`.tmp` then rename) and is an independent unit. Phase 2a adds upload markers + an S3 mirror; don't change the per-file granularity.
- **Offsite is write-only by design (Phase 2a)**: `src/out/offsite.rs` builds the S3 client from EXPLICIT static creds (an `s3:PutObject`-only IAM key), never the ambient/instance chain — a compromised atlas must not be able to read or delete prior evidence. The case dir *is* the durable queue: a file with a sibling `<file>.uploaded` marker is done, a `.tmp` is an in-progress write (skip both); failed PUTs retry next pass with `retry_backoff_secs` backoff. Object immutability is the **bucket's** Object Lock — Argus sets no per-object retention. S3 key = `<prefix>/<case_id>/<rest>` (the path relative to `cases/`). The `upload.routine_frames`/`best_stills` throttles are parsed but near-no-ops today (Phase 2 only persists best stills to disk; `#[allow(dead_code)]` on those fields).
- **`watch` channel**: the engine broadcasts `Option<CaseState>` on every mutation; Phases 3/4 subscribe (`state_tx.subscribe()`), not poll. `main` takes `offsite_rx` (2a) and `outputs_rx` (3) BEFORE `state_tx` moves into `Engine::new`.
- **Phase 3 config guards**: voice spawns iff `overwatch:` is present; PagerDuty spawns iff `pagerduty.routing_key` is non-empty. Either absent disables only that channel — never a hard error. Spawned in DAEMON mode only (not `--once`).
- **Proto symlink + tonic version pin**: `argus/proto/voice.proto` is a *relative* symlink to `../overwatch/proto/voice.proto` (same trick as the HA component). `build.rs` runs `tonic_build::configure().build_server(false).compile(...)`. tonic/prost/tonic-build are pinned to **0.11/0.12/0.11** to match Overwatch's Cargo.toml so the stubs are wire-compatible. Needs `protoc` on PATH at build time (system protoc, as Overwatch's `setup-wsl2.sh` installs).
- **`security_station_notified` (M1 direct-speak)**: Phase 3 CANNOT mutate the engine-owned `CaseState`, so it can't push a `SecurityStationNotified` timeline event for the gate to pick up. For M1, the outputs consumer speaks the security-station line **directly** on `CaseOpened` (when it also fires the PD trigger) rather than via the gate. **Noted future:** a feedback channel back to the engine so the event lands in the timeline → HUD ticker (Phase 4) and the gate, instead of a hard-coded direct line.
- **Daemon vs `--once`**: the daemon assesses **every** configured camera on each trigger; the shipped example configures one (so "exactly one assessment per trigger" holds). Multi-camera fan-out is formalised in Phase 2.

## Versioning

Bump `Cargo.toml` `version` on every deployed change (logged at startup as
`Starting Argus vX.Y.Z`). MAJOR/MINOR/PATCH per the root `CLAUDE.md` policy.
