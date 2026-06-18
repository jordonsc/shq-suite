# Argus

AI-powered alarm assessment daemon. When `alarm_control_panel.shq_alarm`
transitions to `triggered`, Argus captures camera stills, sends them to Claude
with the private premises seed, and logs a natural-language assessment.

**Native x86_64** service that runs on **atlas** (the HA + RAG host) — `cargo
build --release` + systemd, **not** a `cross`/Podman RPi build like nyx/overwatch/dosa.

> **Phase status:** Phases 1 (foundation) and 2 (assessment loop + `CaseState`)
> complete. Offsite resilience (2a), outputs (Overwatch/PagerDuty, Phase 3), the
> kiosk HUD (Phase 4), and the HA component (Phase 5) are later phases. See
> `specs/argus/` (read `00-master.md` first).

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
| `src/config.rs` | `config.yaml` loader. `${VAR}` env expansion, `~` expansion, default path `~/.config/argus/config.yaml`. Adds `loop_config` (cadence/cap) + `telemetry_entities` |
| `src/case.rs` | **The `CaseState` contract** + `LiveAssessment`/`Identification` LLM-output types, their `*_schema()` JSON schemas, `TimelineKind` vocabulary, and `CaseDir` (the on-disk journal = Phase 2a upload queue) |
| `src/engine.rs` | The assessment loop: per-tick multi-camera capture + telemetry → Sonnet live loop → merge into `CaseState` → best stills + Opus forensic; state machine; daily token cap; broadcast + journal |
| `src/version.rs` | `ARGUS_VERSION` from `CARGO_PKG_VERSION` |
| `src/state.rs` | `AlarmState` (Disarmed/Triggered) + `AlarmTracker` → `Transition::{IntoTriggered, OutOfTriggered}` (both edges) |
| `src/ha/mod.rs` | HA client module; `websocket_url()` (http→ws) |
| `src/ha/ws.rs` | HA WS client: auth handshake, `subscribe_events`/`state_changed`, reconnect+backoff, emits `HaEvent::{AlarmTriggered, AlarmCleared}` over an mpsc channel |
| `src/ha/rest.rs` | `RestClient::snapshot(entity)` → JPEG; `state(entity)` + `telemetry(entities)` → per-tick sensor text |
| `src/llm/mod.rs` | `AnthropicClient::assess(&AssessRequest)` → `Completion { text, usage }`. Raw HTTP to `/v1/messages`; multi-image base64 vision, `cache_control` seed, `Reasoning::{Fast,Deep}` (Sonnet effort-low/no-think vs Opus adaptive+high), optional `output_config.format` structured output |

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
- **`watch` channel**: the engine broadcasts `Option<CaseState>` on every mutation; Phases 3/4 subscribe (`state_tx.subscribe()`), not poll. `main` currently drops `_state_rx` — Phases 3/4 keep it.
- **Daemon vs `--once`**: the daemon assesses **every** configured camera on each trigger; the shipped example configures one (so "exactly one assessment per trigger" holds). Multi-camera fan-out is formalised in Phase 2.

## Versioning

Bump `Cargo.toml` `version` on every deployed change (logged at startup as
`Starting Argus vX.Y.Z`). MAJOR/MINOR/PATCH per the root `CLAUDE.md` policy.
