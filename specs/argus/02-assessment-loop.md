# Phase 2 — Assessment Loop & the `CaseState` Contract

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 1** (HA client + Anthropic
> client + trigger detection).
>
> **Status: ✅ IMPLEMENTED & VERIFIED LIVE (2026-06-18).** Multi-camera+telemetry
> loop, tiered Sonnet/Opus, structured `CaseState` + JSON schemas, case-dir
> journal (= the Phase 2a upload queue), `watch` broadcast channel. `--once`
> verified end-to-end (Sonnet structured `CaseState`, cache hit, journal on
> disk). DoD 2 (staged-intruder Opus firm-up) + live alarm trigger deferred — see
> Deviations.
>
> **Goal:** turn the one-shot of Phase 1 into a continuous, multi-camera, telemetry-fed assessment
> loop with the **tiered Sonnet/Opus** model strategy, and produce the **`CaseState`** — the
> structured, evolving record that Phases 3 (voice/PagerDuty) and 4 (kiosk HUD) render. **`CaseState`
> is the central contract of the whole project; design it deliberately.**

## Context

Phase 1 proved a single still → text. Phase 2 builds the brain: every tick while the alarm is
triggered, capture stills from **all** configured cameras plus the **sensor telemetry**, run the
**Sonnet 4.6** live loop for *what/where*, and escalate **best stills** to **Opus 4.8** for forensic
*who* + the dossier. The seed is prompt-cached on both models. The output is a single in-memory
`CaseState` (also journalled to disk) that downstream phases consume — no downstream phase calls the
LLM or HA itself.

## Scope

**In scope:**
- Multi-camera snapshot each tick; sensor-telemetry bundle from HA.
- The **loop**: cadence, prompt assembly (seed cached + per-tick stills/telemetry delta),
  **structured** assessment via `output_config.format` (json_schema).
- **Tiered models:** Sonnet 4.6 live loop; Opus 4.8 forensic ID on best stills + dossier (adaptive
  thinking).
- The **`CaseState`** type + its JSON schema (the contract) + a disk **event log**.
- The full **state machine**: `Disarmed → Triggered → Assessing → … → Disarmed`.
- "Best still" selection per detected intruder.
- Cost/latency guardrails (cadence, max in-flight calls, backoff, daily cap).

**Out of scope:**
- Any *output* (voice, PagerDuty, kiosk) — Phases 3/4 read `CaseState`, Phase 2 only produces it.
- Protect smart-detect "best image" event subscription — **optional** enhancement; note it but
  camera_proxy snapshots at cadence are sufficient for M1.

## The `CaseState` contract (design this first)

A serde-serialisable struct, broadcast as JSON and journalled. Suggested shape — refine, but keep it
the **single source of truth** downstream renders:

```rust
struct CaseState {
    case_id: String,            // stable per alarm episode (dedup key for PagerDuty)
    started_at: DateTime,
    status: CaseStatus,         // Triggered | Assessing | Standdown | Cleared
    summary: String,            // one-line human situation summary (latest)
    threat_level: ThreatLevel,  // Info | Elevated | Critical  (drives voice/PD severity)
    locations: Vec<LocationObservation>, // { camera, label, activity, last_seen }
    intruders: Vec<Intruder>,
    timeline: Vec<TimelineEvent>,         // append-only milestones (for the dossier + HUD ticker)
    updated_at: DateTime,
}
struct Intruder {
    id: String,                 // stable within the case (e.g. "subject-1")
    descriptors: String,        // "male, caucasian, short red hair, tattoo left forearm"
    confidence: f32,
    location: Option<String>,   // current camera label
    activity: Option<String>,   // "attempting to enter the vehicle"
    best_stills: Vec<StillRef>, // ordered best→worst; StillRef { id, camera, captured_at }
}
```
- **`milestones` matter:** Phase 3 maps timeline events → voice lines / PagerDuty updates; Phase 4
  renders intruder cards + the ticker. Emit a timeline event for: case opened, intruder first
  detected, intruder identified (descriptors firmed up), best-still upgraded, security station
  notified, standdown.
- Export the JSON schema (used both for the LLM `output_config.format` *and* for downstream
  validation). Keep `Intruder.id` **stable across ticks** so the HUD doesn't thrash — reconcile new
  observations against existing intruders (the LLM should be told the current roster and asked to
  update/extend it).

## Implementation

### 1. Telemetry bundle (`src/ha/rest.rs`)
- Snapshot all `cameras[]` each tick (concurrently; tolerate a camera being unavailable).
- Pull sensor states relevant to security from HA: door/window/motion binary sensors, DOSA door,
  garage reed switch, etc. Configurable `telemetry_entities[]`. Render as a compact text block
  appended after the cached seed (door X open, motion in Y, etc.).

### 2. The live loop (`src/loop.rs`)
- While `status` is triggered/assessing, every `cadence_secs` (config, default ~5–8 s):
  - Assemble the **Sonnet** request: `system:[seed (cache_control)]`, user content = all current
    stills (base64) + the telemetry text + the **current intruder roster** + the instruction to
    return an updated structured assessment. `output_config.format` = the `CaseState`-delta schema.
  - Merge the structured delta into `CaseState` (reconcile intruders by id; append timeline events;
    update locations/summary/threat_level).
- **Prompt-cache discipline:** the seed is the only cached prefix; everything volatile (stills,
  telemetry, roster, timestamps) goes **after** the breakpoint. Verify `cache_read_input_tokens > 0`
  steady-state. Do **not** interpolate timestamps into the seed.

### 3. Forensic tier (`src/llm/identify.rs`)
- When a new intruder appears, or a higher-quality still arrives, call **Opus 4.8** (adaptive
  thinking) on that intruder's best still(s): produce firm `descriptors` + a richer dossier
  paragraph. This is lower-frequency than the live loop (only on change) to control cost.
- The Opus call uses the **same cached seed** (its own model-scoped cache).

### 4. Best-still selection
- Heuristic for now: prefer the still where the LLM reports the highest identification confidence /
  clearest face/markings; keep an ordered `best_stills` per intruder; upgrade when a better one
  arrives. (Protect smart-detect "best image" is the future upgrade — note it, don't build it.)
- Persist the chosen still bytes to a case dir (`~/.local/share/argus/cases/<case_id>/stills/`) and
  reference by `StillRef.id`; Phase 4 serves these over HTTP.

### 5. State machine + journal (`src/state.rs`)
- `Disarmed → Triggered` (on the HA transition) opens a case (`case_id`, `started_at`).
- `Triggered → Assessing` once the first assessment lands.
- `→ Standdown → Cleared` when the alarm disarms; loop stops, final `CaseState` flushed.
- **Journal** every `CaseState` mutation to an append-only JSONL event log in the case dir (audit +
  crash recovery + later forensic review). **Design the case dir to double as the durable upload
  queue for Phase 2a** (real-time offsite replication): one file per event/still so each is an
  atomic unit Phase 2a can PUT independently and mark uploaded.
- Expose `CaseState` updates via a broadcast channel (`tokio::sync::watch` / `broadcast`) so Phases
  3 & 4 subscribe in-process.

### 6. Guardrails
- `cadence_secs`, `max_concurrent_llm`, exponential backoff on 429/5xx (the API also retries),
  optional `daily_token_cap` that downgrades to a slower cadence when hit. Log cost (`usage`) per call.

## Config additions
```yaml
loop:
  cadence_secs: 6
  daily_token_cap: null
telemetry_entities:
  - binary_sensor.garage_door
  - binary_sensor.front_door
  # ... door/window/motion/DOSA/reed
cameras:                # now the full set, each with a label
  - { entity: camera.garage_camera_high_resolution_channel, label: "Garage" }
  - { entity: camera.front_yard_camera_high_resolution_channel, label: "Front Yard" }
  # ...all 11
```

## Verification (definition of done)
1. Trigger → a `CaseState` opens, populates `locations` from the live cameras, and evolves each tick.
2. A staged "intruder" (someone on a camera) yields a stable `Intruder` with descriptors that **firm
   up** after the Opus pass; the same person across ticks keeps one `id`.
3. `cache_read_input_tokens > 0` steady-state on both models.
4. Disarm → `Cleared`, loop stops, final state + JSONL journal on disk.
5. Cost per tick logged and within the configured cap.

## References
- Phase 1 spec's **Deviations** + **Inputs to Phase 2** (authoritative).
- claude-api skill: `output_config.format` json_schema, prompt-caching placement, adaptive thinking.

---

## Deviations from spec

**Status: ✅ IMPLEMENTED & VERIFIED LIVE (2026-06-18).** `cargo build --release`
warning-free. `--once` verified end-to-end against real HA + a real Anthropic
key: Sonnet structured `CaseState`, prompt-cache hit, case dir journalled.

- **`CaseState` as built** (`src/case.rs`) matches the spec sketch with these
  concretions: `status: CaseStatus {Triggered, Assessing, Standdown, Cleared}`;
  `threat_level: ThreatLevel {Info, Elevated, Critical}` (snake_case on the
  wire); `locations: Vec<LocationObservation{camera, label, activity,
  person_present, last_seen}>`; `intruders: Vec<Intruder{id, descriptors,
  confidence: f32, location: Option, activity: Option, best_stills: Vec<StillRef>,
  identified: bool, dossier: Option, best_camera: Option}>`; `timeline:
  Vec<TimelineEvent{at, kind: TimelineKind, detail}>`; plus `schema_version`
  (=1). `StillRef {id, camera, captured_at}`.
- **The LLM does NOT emit the timeline.** The structured outputs are *deltas*
  (`LiveAssessment` from Sonnet, `Identification` from Opus); Argus reconciles
  them into `CaseState` and **derives the timeline by diffing state**. This gives
  Phase 3 a **closed `TimelineKind` vocabulary** (see Inputs below) rather than
  free-text milestones — more reliable to map to voice lines / PagerDuty.
- **Two structured schemas** (`live_assessment_schema()` /
  `identification_schema()` in `case.rs`), sent as `output_config.format`
  (`{type:"json_schema", schema}`). Structured-output rules obeyed: every object
  `additionalProperties:false`, all properties `required`, "optional" fields are
  nullable-but-required (`["string","null"]`), no numeric/string constraints.
- **Tiered reasoning** via `llm::Reasoning`: the Sonnet live loop uses
  `Fast` (= `thinking:{type:disabled}` + `output_config.effort:low`) for cheap,
  quick ticks; the Opus forensic pass uses `Deep` (= `thinking:{type:adaptive}` +
  `effort:high`). Both send the seed in a `cache_control` system block.
- **Reconciliation strategy**: intruders keyed by stable `id`. The Sonnet prompt
  is given the **current roster** ("reuse these ids for the same people; coin
  `subject-N` for a new one") so ids don't thrash. The fast loop updates
  location/activity/confidence every tick but does **not** overwrite descriptors
  once `identified` (so the firmer Opus descriptors survive). If the model coins
  a `subject-N`, Argus advances its own counter past `N` to avoid collisions.
- **Best-still + Opus heuristic**: a fresh best still is saved (and the Opus pass
  re-run) for an intruder only on **first detection or a confidence improvement
  ≥ 0.1** (`CONF_IMPROVE`), from the camera the live model names in
  `best_camera_label`. `best_stills` is newest-first, capped at 5. This bounds
  Opus cost (it runs only on change, not every tick).
- **Guardrails**: `cadence_secs` (default 6); optional `daily_token_cap` →
  `slow_cadence_secs` (default 30) until UTC midnight; per-call `usage` logged.
  Multi-camera snapshots run **concurrently** (`join_all`); an unavailable camera
  is skipped, not fatal; a tick with zero stills is skipped.
- **`--once`** repurposed (Phase 1's `--once --camera` plain-text path is gone):
  opens an ephemeral case (`alarm_entity = "manual"`), runs ONE full tick, prints
  the `CaseState` JSON, then stands down (exercises the whole pipeline + journal
  without arming).
- **Cache numbers observed (live, 2026-06-18)**: `seed.example.md` is now ~4060
  bytes ≈ **2265 input tokens**, which **clears Sonnet's 2048-token floor** — run
  1 `cache_read=0`, run 2 `cache_read=1867` (hit). ⚠️ **But Opus 4.8's cache floor
  is 4096 tokens**, so the *real premises seed must exceed ~4096 tokens* for the
  Opus path to cache (Sonnet only needs 2048). Per-tick cost (one camera, no
  intruder): ~2265 in / ~110 out tokens on Sonnet.
- **Verified vs deferred**: DoD 1 (case opens, locations populate, evolves) ✓;
  DoD 3 (cache hit steady-state) ✓ (Sonnet); DoD 4 state machine + JSONL journal
  on disk ✓ (via `--once` standdown → `cleared`, manifest + events/*.json +
  state.json + dossier.json written); DoD 5 cost logged ✓. **DoD 2 (a staged
  intruder → stable id firming up after the Opus pass) is DEFERRED** — the
  residence is asleep and staging a person on camera is out of bounds this
  window; the Opus path is shape-verified (compiles, schema valid) and correctly
  does **not** fire when no person is present. **A live alarm trigger** (vs
  `--once`) is also deferred (won't trip the real house alarm; the WS edge
  detection — `IntoTriggered`/`OutOfTriggered` — is unit-correct).

## Inputs to Phases 2a, 3 & 4

Concrete API surface downstream phases inherit (all in `argus/src/`):

- **Broadcast channel** (`main.rs`): a `tokio::sync::watch::Sender<Option<CaseState>>`
  is created in `main` and passed to `Engine::new`. The engine `send`s
  `Some(state.clone())` on **every mutation** (case open, each tick, standdown).
  Phases 3/4 take a `watch::Receiver<Option<CaseState>>` (`tx.subscribe()`),
  `await rx.changed()`, and render the latest `CaseState`. `None` = no active
  case. To wire a consumer in, `main` must clone the receiver before moving
  `state_tx` into the engine (today it drops `_state_rx`; Phases 3/4 keep it).
- **Subscribed JSON shape** = `CaseState` serialised by serde (snake_case enums).
  This is the SAME shape journalled to disk and (Phase 2a) replicated to S3 —
  one schema, three surfaces.
- **Case dir layout** (`case::CaseDir`, base `~/.local/share/argus`):
  ```text
  cases/<case_id>/
    manifest.json          # {case_id, started_at, schema_version, alarm_entity}
    events/000001.json …   # one FULL CaseState snapshot per mutation (atomic, seq-numbered)
    stills/<still_id>.jpg   # best stills (StillRef.id is the file stem)
    state.json             # latest CaseState (overwritten each mutation)
    dossier.json           # final CaseState on standdown
  ```
  - **`case_id`** = `case-<UTC %Y%m%dT%H%M%SZ>` — stable per episode, the
    PagerDuty dedup key and the S3 case prefix.
  - **Per-event/per-still granularity for Phase 2a**: every `events/NNNNNN.json`
    and `stills/*.jpg` is written atomically (`.tmp` then rename) and is
    independently PUT-able; any prefix of events is a coherent partial case.
    `CaseDir` is the durable record AND the upload queue — Phase 2a adds upload
    markers and an S3 mirror of this exact tree (`CaseDir::append_event` /
    `save_still` / `write_dossier` are the write points to hook).
  - **Phase 4 serves** `stills/<id>.jpg` at `/stills/<id>.jpg`; `StillRef.id` is
    the stem (no extension).
- **Timeline-event vocabulary** (`case::TimelineKind`, snake_case on the wire) —
  the closed set Phase 3 maps to voice lines / PagerDuty updates and Phase 4
  renders in the ticker:
  | `TimelineKind` | Emitted when |
  |---|---|
  | `case_opened` | Alarm triggered, case opened |
  | `intruder_detected` | A new `subject-N` first appears |
  | `intruder_identified` | The Opus pass firmed up an intruder |
  | `best_still_upgraded` | A clearer still was captured for an intruder |
  | `threat_level_changed` | `threat_level` changed |
  | `security_station_notified` | **Reserved for Phase 3** to emit when it PUTs to PagerDuty (not emitted by Phase 2) |
  | `standdown` | Alarm disarmed |
  | `cleared` | Case terminal |
  - Phase 3's **positive-only** voice gate should whitelist `case_opened`,
    `intruder_identified`, `best_still_upgraded`, `security_station_notified`
    (affirmatives); `threat_level_changed` → only when escalating; never speak on
    a `None`/failure. `ThreatLevel::pagerduty_severity()` (already on the enum)
    maps Info/Elevated/Critical → info/warning/critical.
- **Engine entry points** (`engine::Engine`): `new(cfg, rest, sonnet, opus, seed,
  state_tx)`, `run(rx: mpsc::Receiver<HaEvent>)` (daemon), `run_once()` (manual).
  Phases 3/4 do **not** call the engine — they only subscribe to the channel.
  `HaEvent::{AlarmTriggered, AlarmCleared}` now carries both edges.
