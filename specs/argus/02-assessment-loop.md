# Phase 2 — Assessment Loop & the `CaseState` Contract

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 1** (HA client + Anthropic
> client + trigger detection).
>
> **Status: 📝 NOT STARTED.**
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
_(Implementing agent: record the final `CaseState` schema as built, the actual reconciliation
strategy, cadence/cost numbers observed, and any LLM-prompting that was needed to keep intruder ids
stable.)_

## Inputs to Phases 2a, 3 & 4
_(Implementing agent: document the `CaseState` broadcast channel API, the exact JSON shape consumers
subscribe to, the case-dir/still-path layout (Phase 4 serves it, Phase 2a replicates it), the
per-event/per-still file granularity Phase 2a's uploader relies on, and the timeline-event vocabulary
Phase 3 maps to voice/PagerDuty.)_
