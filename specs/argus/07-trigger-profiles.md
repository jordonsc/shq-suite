# Phase 7 — Trigger Profiles (tiered response)

> Sub-spec of the [Argus master plan](./00-master.md). **Status: 🟢 4a + 4b LANDED
> + precision pass (0.31.0).** Phase 4a (argus 0.28.0) laid the rails —
> `TriggerProfile` + per-source config + the dormant output/kiosk gate, all
> defaulting to `Alarm` so an existing case is byte-for-byte unchanged. **Phase 4b
> (argus 0.29.0)** makes the softer profiles work: the escalation/promotion engine,
> softer-trigger firing, the real-alarm trip, and profile-parameterised prompts.
> **Locked user decisions (2026-06-19):** a self-escalated case goes to FULL `Alarm`
> posture **including the klaxon**, AND Argus **trips the real
> `alarm_control_panel`** (cascading the legacy whole-house automations). No opt-out
> flags. **Live-fire of a real escalation (which trips the whole-house alarm) is a
> MORNING task with the user present** — 4b is unit-tested + `--once`-validated only;
> the real perimeter/front-door HA entities are user-owned and NOT wired yet.
> **Promoted into M1** (2026-06-19): the
> tiered response is no longer a deferred M2 idea but a first-class M1 deliverable.
> Supersedes the parked "Trigger TYPES `[alert, investigate]`" open finding in
> [`06-refinements.md`](./06-refinements.md) and the M2 "perimeter-security" note —
> generalised to **three profiles**. Argus stops being a single alarm-assessor and
> becomes a **tiered security assessor** driven by *why* it was woken. Ledger:
> [`shq-suite-0002`](ledger).

## Why

Today Argus has exactly one entry point — `alarm_control_panel.shq_alarm` →
`triggered` — and one posture: **an intrusion is in progress, respond fully**
(klaxon, breach voice, PagerDuty, kiosk takeover, threat ratchet). That is correct
for a real alarm but too blunt for the two softer signals the premises also
generates: *perimeter activity while the house is secured* and *someone walking up
to the front door*. Both deserve an AI look, but **neither should fire the klaxon or
page a responder on sight** — Argus should assess first and only escalate if it
concludes a genuine threat.

So a case now carries a **trigger profile** that selects its initial posture and,
crucially, **whether its outputs are gated** until Argus confirms a threat.

## The three profiles

### `Alarm` — confirmed intrusion (today's behaviour)
- **Trigger:** the real house alarm (`alarm_control_panel.shq_alarm` → `triggered`).
- **Posture:** assume an intrusion is in progress.
- **Initial threat:** `Elevated` (ratchets up on weapons/aggression; decays on
  inactivity; closes on disarm / auto-close).
- **Outputs:** IMMEDIATE + full — klaxon (`SetAlarm` when `klaxon_enabled`), the
  breach voice line on `CaseOpened`, PagerDuty trigger, kiosk takeover. Unchanged
  from 0.22.x.
- This profile is the current implementation; the others are *added beside* it.

### `Investigate` — perimeter security (smart pre-alarm)
- **Trigger:** a person/vehicle on **external** cameras while the house is in a
  secured/inactive posture — armed-away, or outside permitted outdoor hours. The
  existing telemetry timers gate this: `timer.outdoor_sensor_permitted` (are people
  expected outside right now) and `timer.perimeter_security_cooldown` (the
  debounce/assessment window). Not the main alarm.
- **Goal — the smart-alarm decision:** *are residents in danger? are the people
  present non-residents only?* Distinguish a resident arriving home / a known
  visitor / a delivery from someone loitering, trying doors and windows, masked, or
  otherwise behaving like a prowler.
- **Initial threat:** `Info` (low).
- **Outputs:** **GATED** — no klaxon, no breach voice, no PagerDuty, no kiosk
  takeover **while investigating**. Argus still journals + broadcasts `CaseState`
  (the HUD / HA component can show "investigating"), but stays silent outward.
- **Resolution:**
  - **Escalate** → if Argus concludes a real threat (a non-resident behaving as an
    intruder, an attempted entry, a weapon, or a resident in apparent danger): the
    case is **promoted to `Alarm`** — full outputs fire from that point (including a
    now-justified breach line) and Argus optionally trips the real HA alarm.
  - **Stand down** → if it concludes benign (resident/known person, no threat) or the
    cooldown window expires with nothing of concern: close quietly, no outputs.
- This is the "smart alarm trigger": **Argus is the decision-maker** on whether
  perimeter activity becomes an alarm, instead of a dumb sensor firing the siren.

### `General` — benign trigger (fast triage)
- **Trigger:** a low-stakes, expected event — a person approaching the **front door**
  (or a similar friendly-zone signal: doorbell, porch motion).
- **Goal:** a quick check for **obvious** threat indicators only — a balaclava / face
  concealment, a visibly brandished weapon, forced-entry behaviour. NOT a deep
  who-are-they identification.
- **Initial threat:** `Info`.
- **Outputs:** **GATED** (silent), same as `Investigate`.
- **Resolution — fast:** a short, shallow look (a couple of ticks, lightweight —
  ideally the live Sonnet pass on the trigger camera(s) only, no forensic Opus
  unless escalated).
  - **Escalate** → an obvious threat indicator promotes the case to `Investigate`
    (assess further) or straight to `Alarm` (a weapon / balaclava at the door).
  - **Stand down quickly** → nothing obvious (a delivery, a resident, an ordinary
    visitor): auto-close within seconds.

### Profile comparison

| | `Alarm` | `Investigate` | `General` |
|---|---|---|---|
| Trigger | house alarm | perimeter activity while secured | benign approach (front door) |
| Assume | intrusion in progress | possible prowler | benign visitor |
| Initial threat | Elevated | Info | Info |
| Klaxon / voice / PD / kiosk | **on immediately** | **gated** until escalated | **gated** until escalated |
| Depth | full live + forensic | full live + forensic | shallow / fast, Sonnet-first |
| Default end | disarm / auto-close | escalate → `Alarm`, else quiet standdown | escalate, else fast standdown |

`General` can escalate into `Investigate` or `Alarm`; `Investigate` can escalate
into `Alarm`. Escalation only ever moves *up* the response ladder; it never
de-escalates a confirmed `Alarm` (the existing threat ratchet still owns that).

## How the profile reaches Argus

Argus currently watches a single `alarm_entity` for `triggered`. Profiles need
**multiple trigger sources, each tagged with a profile.** Two mechanisms (likely
both):

1. **Per-source profile in config.** Extend the watched-trigger config so each
   source declares its profile, e.g. the alarm entity → `Alarm`, a perimeter
   template/group → `Investigate`, a front-door approach sensor/event → `General`.
2. **A profile hint entity** (mirrors `trigger_location_entity`): an
   `input_select`/`input_text` the HA automation sets alongside the trigger, read
   once at case open. Useful when one mechanism fans several sub-cases.

The HA side owns *classifying* the trigger (it already sets
`input_text.alarm_trigger_room`); Argus owns *assessing and deciding*.

## Implementation sketch (for the future build)

- **`CaseState.trigger_profile: TriggerProfile`** — `enum { Alarm, Investigate,
  General }`, `#[serde(default)]` to `Alarm` for back-compat with existing journals.
  Read at case open, drives the initial `threat_level` and the output gate.
- **Output gating.** The Phase-3 outputs consumer (`src/out/mod.rs`) gates the
  voice/klaxon/PagerDuty/takeover channels on the profile: for `Investigate` /
  `General` they are suppressed until the case is **promoted**. Promotion is a new
  milestone (e.g. `TimelineKind::Escalated`) that flips the case's effective profile
  to `Alarm`; from then the outputs flow (and a retroactive breach line can fire).
  The HUD/record channels are never gated — they always reflect the live assessment.
- **Escalation policy in the engine.** During assessment Argus decides escalate vs
  stand down per the profile rules (LLM-assessed threat + resident certainty +
  behaviour). `General` is shallow and short (few ticks, fast auto-close, Sonnet-only
  until escalated); `Investigate` runs the full loop but silent, bounded by the
  perimeter cooldown timer. A weapon / forced entry / resident-in-danger always
  escalates.
- **Seed + prompts.** The premises seed gains per-profile guidance (what "benign"
  looks like at the front door, who the residents/known visitors are, what perimeter
  behaviour is suspicious). The live instruction is parameterised by profile so the
  model knows whether it is hunting an intruder (`Alarm`), vetting a prowler
  (`Investigate`), or glancing for obvious danger (`General`).
- **Voice gate.** `voice_policy` is unchanged in spirit (positive-only); it simply
  never sees events for a gated case until promotion. An escalation may speak a
  first line ("Security breach detected…") at promotion rather than at case open.

## Phase 4b — what landed (argus 0.29.0)

- **Promotion (`engine.rs`).** `ActiveCase.{escalated, investigate_deadline}`; a
  per-tick `decide_promotion(active) -> {Continue, Promote(reason), Standdown}`
  (pure, unit-tested) consumed by `evaluate_promotion` at the end of `run_tick`.
  `promote()` is idempotent (guards on `escalated`): flips `effective_profile` to
  `Alarm`, pushes `TimelineKind::Escalated`, broadcasts the now-ungated state, then
  **trips the real alarm** — see the standdown-robustness pass (0.32.0) below for
  the script-based trip; either way the resulting `Triggered` is no-op'd by `handle`
  (a case is already active → no double-open; the existing 4a guard).
- **Escalation rules.** Always-escalate (any profile): armed intruder / `Critical`
  / resident-in-danger. `General`: also escalate on an OBVIOUS threat
  (weapon/balaclava/forced-entry, scanned from the free-text `threats`+`summary`),
  else quiet standdown after `GENERAL_STANDDOWN_TICKS` (4); Opus forensic is
  **skipped** for a non-escalated `General` (Sonnet-only, cost). `Investigate`:
  escalate on a detected intruder behaving as a prowler, else quiet standdown when
  `investigate_deadline` (`INVESTIGATE_DEADLINE_SECS`, 120 s) passes.
- **Outputs on `Escalated`.** `route_event` treats `Escalated` like a real
  case-open: the breach voice line (`voice_policy::lines_for` shares the
  `CaseOpened` arm), klaxon-on (`VoiceMsg::Alarm(true)`), and `pd.trigger()`. After
  promotion `gated()` is false, so later events route normally and the kiosks
  consumer takes over on its next tick.
- **Softer-trigger firing (`ha/ws.rs`).** The WS now subscribes to ALL
  `triggers[].entity`; a non-alarm source's inactive→active edge emits
  `HaEvent::TriggerFired{entity_id, profile}` → `handle` opens a gated case (no-op
  if one is already active). The alarm entity keeps the full `AlarmMode` machine.
  The live atlas config has no `triggers:` block, so this path is inert there.
- **Prompts + testing.** `live_instruction_camera` is parameterised by
  `effective_profile` (hunt / vet-prowler / glance-for-obvious-danger); per-profile
  seed guidance placeholders added to `seed.example.md`. A `--profile
  <alarm|investigate|general>` override on `--once` exercises the gated/escalation
  paths without real softer-trigger HA entities.

## Precision pass (argus 0.31.0 — production false-escalation hardening)

The house is now in PRODUCTION (real PagerDuty; a self-escalation trips the whole
house). Two changes reduce false-escalation risk on the gated profiles without
touching the real `Alarm` path:

- **≥2-tick escalation persistence.** A gated→`Alarm` promotion now requires the
  escalate condition to hold for **≥`ESCALATION_PERSISTENCE_TICKS` (2) CONSECUTIVE
  ticks**. A single tick's read (e.g. a phone misread as a blade for one frame)
  must not page + cascade the whole house. `ActiveCase.escalation_streak`
  increments on a `Promote` verdict and resets to 0 on `Continue`/`Standdown`, so
  an intermittent signal (met, not-met, met) never reaches the threshold. The split
  is `decide_promotion` (per-tick verdict, pure) → `apply_persistence` (the streak
  gate, pure, unit-tested) → `evaluate_promotion`. **Gated-ONLY:** `decide_promotion`
  returns `Continue` for an already-`Alarm`/escalated case, so the streak never
  increments on the real Alarm path — the threat ratchet, armed/weapon threat-floor,
  and immediate outputs are unchanged (zero added latency; the pre-existing
  Alarm-path tests pass byte-identical). The `General` fast auto-close still applies.
- **Time + arm-state context (priors, NOT triggers).** `live_instruction_camera`
  injects the current **local date/time + day-of-week** (in `cfg.timezone`, via
  `chrono-tz` — the container runs UTC, never `chrono::Local`; invalid tz → UTC +
  warn) and the current **alarm arm-state** into every per-tick prompt (all
  profiles; harmless for `Alarm`). The model is told these inform suspicion but do
  not decide it: a resident arriving home at an odd hour is still a resident; a
  door/perimeter approach while armed-away weights suspicion higher. "Recent
  authorised front-door access" is not special-cased — the operator adds
  `event.front_door_access` to `telemetry_entities` and the model judges its
  timestamp against the injected current local time.

## Resolved open questions (Phase 4b)

- **HA trigger wiring + real-alarm trip:** softer sources are per-`triggers[]`
  config entities; on escalation Argus DOES trip the real `alarm_control_panel`
  (locked decision).
- **Thresholds:** `General` budget = 4 ticks; `Investigate` silent window = 120 s
  (a real perimeter-cooldown timer can replace the default later). Obvious-threat /
  prowler keyword scans over the model's free-text observations.
- **Klaxon on promotion:** YES — a promoted case fires the FULL alarm set including
  the klaxon (locked decision; no voice+PD-only softer escalation).
- **Cost shape:** `General` stays Sonnet-only + short — no Opus unless escalated.

## Morning live-fire checklist (user present)

- ⚠️ The real perimeter/front-door HA entities are **user-owned** — add them to a
  `triggers:` block in atlas's `~/.config/argus/config.yaml` before any softer-path
  live test.
- A real escalation **trips `alarm_control_panel.shq_alarm`** → cascades the legacy
  whole-house automations (kiosks 02–10, DOSA) + (when enabled) the klaxon. Do this
  ONLY with the user present and the test posture understood.
- Validate `--once --profile investigate` / `--profile general` against atlas
  (needs `ANTHROPIC_API_KEY`) for a gated `CaseState` first (no Opus, fast close),
  THEN a controlled softer-trigger fire, THEN a controlled escalation.

## Standdown robustness pass (argus 0.32.0 — kill switch + script trip)

Triggered by a real failure: an escalation tried to trip the manual alarm panel
but it was **DISARMED**, so the panel ignored the trip — there was no `triggered`
state to disarm and no off-switch. The klaxon / PagerDuty / kiosk ran with no way
to stop them except restarting Argus (which left PagerDuty un-resolved). Four
panel-independent fixes:

- **Panel-independent STANDDOWN control (the kill switch).** `standdown_entity`
  (top-level config, an `input_button` like `input_button.argus_stand_down`): ANY
  `state_changed` on it (a press changes its state to a fresh timestamp → treated as
  a press) emits `HaEvent::StandDownRequested`; `engine::handle` calls the existing
  `standdown()` if a case is active (klaxon off + PD resolve + kiosks restore),
  logging either way. So an escalation can be cancelled WITHOUT a panel that ever
  reached `triggered`. **A disarm OR the standdown control both stand the case down.**
- **Klaxon-off on ANY case-clear.** `out::route_event` now stops the klaxon
  out-of-band (`voice.set_alarm(false)`) on a `Standdown`/`Cleared` TIMELINE event —
  not just the alarm-mode active→disarmed edge `standdown_voice` covered — so a
  control / deadline / kill-switch standdown (no mode edge) silences the siren. It is
  idempotent (a redundant `SetAlarm(false)` is harmless) and runs alongside the
  mode-edge path + the independent PD `resolve`.
- **Stable PagerDuty dedup key.** `pagerduty.dedup_key` (default `"argus-shq-alarm"`,
  NOT the per-case `case_id`) is used for every trigger/update/ack/resolve. One alarm
  incident at a time (correct for a single home), and — crucially — **HA can resolve
  the same page itself (same key) even if Argus is down**, so a page left open by a
  crashed/restarted Argus is still closable from HA.
- **Script-based panel trip.** `escalation_alarm_script` (top-level config, e.g.
  `argus_alarm` → `script.argus_alarm`): when set, `promote()` trips the panel by
  calling that script (which arms/triggers the panel WITH the secret code, HA-side)
  instead of `alarm_control_panel.alarm_trigger` — the trip code lives in HA, not in
  Argus's config/seed. Unset = the direct `alarm_trigger` fallback (unchanged). **The
  no-loop guard is intact:** `mark_promoted` (idempotent on `ActiveCase.escalated`)
  flips the case ungated FIRST, so the script-raised panel `Triggered` is RECONCILED
  by `handle` (a case is already active → no second case, no re-open).
