# Phase 3 — Outputs: Overwatch Voice & PagerDuty Security Station

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 2** (`CaseState` + its
> broadcast channel + timeline-event vocabulary).
>
> **Status: ✅ IMPLEMENTED & SHAPE-VERIFIED (2026-06-18); LIVE-FIRE DEFERRED.**
> Overwatch gRPC voice client (tonic 0.11, proto symlinked from overwatch), the
> positive-only gate (pure, unit-tested), the PagerDuty Events v2 client
> (`build_event` split from `send` for shape-testing), and the `out::run` wiring
> (timeline-diff → independent voice + PD channels) are all built and compile
> warning-free; 9 unit tests pass. **No live-fire**: no real `SetAlarm`/
> `Verbalise`/klaxon and no real PD page were sent (residence asleep). Owed for
> sign-off: Overwatch reachable on the LAN + a real `PAGERDUTY_ROUTING_KEY`.
>
> **Goal:** act on `CaseState`. Speak **positive-only** progress through Overwatch to intimidate the
> intruder, and dispatch/maintain a dossier on the security station (PagerDuty for M1).

## Context

Phase 2 produces an evolving `CaseState`; Phase 3 are the first two consumers. Both subscribe to the
`CaseState` broadcast channel and react to **timeline events** — they do not call the LLM or HA. The
voice channel is deliberately one-sided: it announces wins to unsettle the intruder ("Security
station has received your image. Intruder identified.") and **never** verbalises failures, gaps, or
uncertainty.

## Scope

**In scope:**
- Overwatch gRPC client (`tonic`, reuse `overwatch/proto/voice.proto`).
- The **positive-only policy gate**: a pure function mapping timeline events → spoken lines, that
  *structurally cannot* emit a negative/failure line.
- Alarm klaxon control via Overwatch `SetAlarm`.
- PagerDuty Events API v2 client (`reqwest`): trigger on case open, update on material change,
  resolve on standdown — dossier in the payload.
- A milestone→action mapping table wiring `CaseState` timeline events to both channels.

**Out of scope:**
- The kiosk HUD — Phase 4.
- Two-way security-station comms / acknowledgement flow — future (M1 is fire-and-forget PD).

## Implementation

### 1. Overwatch gRPC client (`src/out/voice.rs`)
- `tonic-build` compiles `voice.proto` (symlink `overwatch/proto/voice.proto` → `argus/proto/`,
  same trick the HA component uses). Connect to Overwatch on `host:50051` (config).
- On case open: `SetAlarm { alarm_id: "security", enabled: true, volume }` to start the klaxon loop;
  `SetAlarm { enabled: false }` on standdown.
- `Verbalise { text, notification_tone_id?, voice_id?, volume }` for spoken lines.

### 2. The positive-only policy gate (`src/out/voice_policy.rs`)
- Input: a timeline event (+ relevant `CaseState` context). Output: `Option<SpokenLine>` — `None`
  for anything that isn't an affirmative milestone.
- **Whitelist of speakable milestones only** (closed set), e.g.:
  | Timeline event | Spoken line (illustrative) |
  |---|---|
  | `case_opened` | "Intruder detected. Security protocol engaged." |
  | `intruder_identified` | "Intruder identified." (optionally + descriptor summary) |
  | `security_station_notified` | "Security station has received your image and location." |
  | `best_still_upgraded` | "Clear image captured." |
  | `standdown` | (silence, or an authorised all-clear if a resident disarmed) |
- **Failure modes (LLM uncertain, camera offline, PD send failed, low confidence) map to `None`** —
  they are logged, never spoken. The intent is intimidation; revealing gaps undermines it.
- Debounce/rate-limit speech so milestones don't talk over each other; queue and pace.

### 3. PagerDuty Events v2 (`src/out/pagerduty.rs`)
- `POST https://events.pagerduty.com/v2/enqueue`, body:
  ```json
  { "routing_key": "<PAGERDUTY_ROUTING_KEY>",
    "event_action": "trigger",
    "dedup_key": "<case_id>",
    "payload": { "summary": "<CaseState.summary>", "source": "argus@atlas",
      "severity": "critical",
      "custom_details": { "threat_level": "...", "intruders": [...], "timeline": [...] } } }
  ```
- **trigger** on case open; re-send `trigger` with the same `dedup_key` on material updates (new
  intruder, firm identification) so the incident's latest dossier reflects current state;
  **`resolve`** (same `dedup_key`) on standdown. Optionally attach best-still URLs (Phase 4 serves
  them) or links once available.
- `severity` derived from `CaseState.threat_level`.

### 4. Wiring (`src/out/mod.rs`)
- Subscribe to the `CaseState` channel; on each update, diff the timeline for new events and route
  them through the voice gate and the PagerDuty updater. Keep the two channels independent (a PD
  failure must not block speech and vice-versa; both log on failure).

## Config additions
```yaml
overwatch:
  host: "<overwatch-host>"
  port: 50051
  alarm_id: "security"
  voice_id: "Amy"
  volume: 1.0
pagerduty:
  routing_key: "${PAGERDUTY_ROUTING_KEY}"
  source: "argus@atlas"
```

## Verification (definition of done)
1. Trigger → klaxon starts; PagerDuty incident appears with the opening dossier.
2. As the case evolves, **only** affirmative lines are spoken (verify by forcing a low-confidence /
   camera-offline path — it must stay silent), and the PD incident's details update.
3. Disarm → klaxon stops, PD incident resolves.
4. PD send failure or Overwatch unreachable is logged and does not crash the loop or speak a failure.

## References
- `overwatch/CLAUDE.md`, `overwatch/proto/voice.proto`, `overwatch/README.md` (gRPC API, volume
  semantics, `SetAlarm` loop behaviour).
- `home-assistant/custom_components/overwatch/proto/` (the symlink-the-proto precedent).
- Phase 2 **Inputs to Phase 3 & 4** (the timeline-event vocabulary — authoritative).

---

## Deviations from spec

- **Speakable-milestone whitelist (final):** `case_opened` → "Intruder detected.
  Security protocol engaged."; `intruder_identified` → "Intruder identified."
  (+ the latest identified intruder's descriptor if present);
  `best_still_upgraded` → "Clear image captured."; `security_station_notified` →
  "Security station has received your image and location.";
  `threat_level_changed` → "Threat level elevated/critical." **only on an
  escalation** (de-escalation is silent). **`intruder_detected` is deliberately
  NOT spoken** (the case-open line already announced the protocol; per-subject
  detection chatter is left to the HUD and would reveal we're still counting
  people). `standdown`/`cleared` → silent. The gate is a whitelist `match` —
  structurally incapable of a negative line; failure modes are never timeline
  events so they have no path to the gate.
- **Klaxon** is bound to case lifecycle, not a spoken line: `SetAlarm(enabled:
  true)` on `case_opened`, `SetAlarm(enabled: false)` on `standdown`/`cleared`.
  It and speech share ONE serial voice worker (klaxon tunnelled as a sentinel) so
  a stop can't race a `Verbalise`.
- **PD update cadence:** `trigger` (same dedup_key = `case_id`) on `case_opened`,
  `intruder_detected`, `intruder_identified`, `threat_level_changed` (material
  changes refresh the dossier); `resolve` on `standdown`/`cleared`.
  `best_still_upgraded`/`security_station_notified` do NOT re-page on their own.
- **`security_station_notified` (M1):** Phase 3 cannot mutate the engine-owned
  `CaseState`, so it does NOT emit a `SecurityStationNotified` timeline event.
  Instead the consumer speaks the security-station line **directly** on
  `case_opened` (alongside the PD trigger). Wiring a feedback channel so the event
  lands in the timeline (→ HUD ticker + gate) is a noted future. The reserved
  `TimelineKind::SecurityStationNotified` is still mapped in the gate for when
  that channel exists.
- **No presigned URLs:** the PD `custom_details.evidence_s3_prefix` is the BARE
  `s3://<bucket>/<prefix>/<case_id>/` (per 02a — atlas has write-only creds);
  responders mint a presigned GET out-of-band with the admin principal.
- **Build:** tonic/prost/tonic-build pinned to **0.11/0.12/0.11** (= Overwatch);
  `proto/voice.proto` is a relative symlink to `../overwatch/proto/voice.proto`;
  `build.rs` builds the **client only** (`build_server(false)`). System `protoc`
  (libprotoc 34.1) used. PagerDuty reuses argus's existing `reqwest` (rustls) — no
  new HTTP dep.

## Inputs to Phase 4

- **PD payload uses NO best-still HTTP URLs yet** — only the bare S3 case prefix.
  When Phase 4 serves `/stills/<id>.jpg`, the PD `custom_details` could additionally
  carry those URLs; if so the URL scheme must match Phase 4's HTTP server.
- **HUD ticker = the same `TimelineKind` vocabulary** Phase 3 maps. Phase 4 should
  render every kind (including `security_station_notified` once the engine emits
  it — see the M1 direct-speak deviation; today the HUD will NOT see that event
  because Phase 3 speaks it directly without a timeline write).
- **Voice + HUD share the case_id cursor pattern** (diff `timeline` length per
  `case_id`) — Phase 4 can reuse `out::run`'s diffing approach.
