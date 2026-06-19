# Phase 7 — Trigger Profiles (tiered response)

> Sub-spec of the [Argus master plan](./00-master.md). **Status: 📋 DESIGN COMPLETE
> — M1 SCOPE, pending implementation.** **Promoted into M1** (2026-06-19): the
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

## Open questions (resolve at build time)

- Exact HA trigger wiring for `Investigate` / `General` (which entities/events, and
  whether Argus trips the real `alarm_control_panel` on an `Investigate` escalation
  or only runs its own outputs).
- The escalation criteria thresholds per profile (how "obvious" must a `General`
  threat be; how long an `Investigate` may run silent before auto-standdown).
- Whether a promoted case fires the klaxon, or escalates to voice + PD only (the
  klaxon may be too aggressive for an `Investigate` confirmation vs a real `Alarm`).
- Cost shape: `General` should be cheap (it will fire often — every doorstep
  visitor); confirm it stays Sonnet-only + short.
