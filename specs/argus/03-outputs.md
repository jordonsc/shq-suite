# Phase 3 — Outputs: Overwatch Voice & PagerDuty Security Station

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phase 2** (`CaseState` + its
> broadcast channel + timeline-event vocabulary).
>
> **Status: 📝 NOT STARTED.**
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
_(Implementing agent: record the final speakable-milestone whitelist, PD update cadence, and any
Overwatch/PD quirks.)_

## Inputs to Phase 4
_(Implementing agent: note anything Phase 4 should mirror — e.g. if best-still URLs are referenced in
PD payloads, the URL scheme must match what Phase 4's HTTP server serves.)_
