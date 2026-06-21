# Phase 8 — post-live-test hardening (forensics-in-alarm, HUD, disarm latency)

> **Status: ✅ DONE + LIVE-FIRE VALIDATED (2026-06-21, argus 0.36.0).** The original
> Phase-8 defects #1–#6 were implemented at 0.34.0; the work then **evolved through a
> series of user live-fire walk-throughs** (full arm → trigger → assess → disarm, with
> Overwatch voice + klaxon and PagerDuty live) to **0.36.0**, all on `argus-design-specs`.
> Confirmed working end-to-end against the real alarm: non-blank priority MAIN image,
> Opus ID + mugshots + zone-movement, casualty/injury escalation, fast disarm→AUTHORISED,
> the arming warning sign, the box-vs-knife weapon model, and the silent Investigate
> flow on real deliveries. Out-of-scope #6.x (the alarm-code script / standdown entities)
> remain user-owned + unset, by design. Follows the 0.33.x two-flows + persons refactor
> ([`07-trigger-profiles.md` §Phase 8](./07-trigger-profiles.md)).
>
> **Post-0.34.0 follow-ups (all live-fire validated, see ledger `shq-suite-0002`):**
> - **0.34.1** — `Person::warrants_attention` (structured danger overrides resident
>   classification: an armed person is ID'd/carded/zone-tracked even if read as a
>   resident); per-frame MAIN-image rank (a held weapon shot isn't displaced by an
>   unarmed/empty frame); HUD main-image preload (no flicker).
> - **0.34.2** — Investigate prompts treat kitchen cutlery for food prep as benign.
> - **0.34.3 → 0.34.5** — kiosk keep-awake state machine (pin during Arming/Triggered/
>   live case; blank while merely armed; reset the idle timer at standdown); arming
>   screen shows a pulsing **warning sign** (not the calm ring); life-threat headline
>   pulse removed (broke layout), vignette/border kept.
> - **0.34.6** — HUD cache-busting (timestamped asset URLs + `no-store`) so kiosks
>   never serve stale CSS/HTML after a deploy.
> - **0.34.7** — `armed` narrowed to real weapons only (a carried box no longer flags);
>   kiosk text-selection disabled.
> - **0.35.0** — **confidence scores**: `weapon_confidence` (→ `armed` via tunable
>   `ARMED_FLOOR` 0.4) replaces the binary, and `presence` (→ `PRESENCE_FLOOR` 0.4)
>   filters hallucinated subjects; degenerate "no person" Opus IDs are suppressed.
> - **0.36.0** — **all LLM prompts externalised to `argus/prompts.yaml`** (baked in,
>   runtime-overridable at `~/.config/argus/prompts.yaml`); Alarm flow now treats a
>   held weapon as serious regardless of apparent "food prep" (the chef's-knife miss).
>   Empirically confirmed: box `weapon_confidence` 0.05 (benign) → knife 0.45→0.82
>   (armed → life_threatening).

## Environment & ground truth (a fresh agent won't know this)

- **Repo:** `/mnt/t/Repos/shq-suite`, branch **`argus-design-specs`** (the 0.33.x work
  is committed-or-in-tree here; do NOT merge to `main`).
- **Deploy target:** atlas (`jordonsc@atlas.shq.sh`, key `~/.ssh/jordon.pem`),
  Argus runs as a rootless Podman Quadlet `--user` service. Deploy with
  `argus/deploy-container.sh` (builds on atlas, ~90 s). Currently running **v0.33.1**.
- **Test posture (safe to deploy + run, overnight):** Overwatch amp is **OFF** (no
  audible TTS/klaxon), PagerDuty is in **maintenance** (events accepted, no paging).
  Both remain so overnight.
- **HA API:** `$HA_URL` / `$HA_TOKEN` in env; `./ha get|post <path> [body]` helper,
  or `curl`. Automations live in the HA DB (edit via `POST /api/config/automation/config/<id>`).
- **DO NOT trigger the real house alarm / arm the panel autonomously.** You cannot
  walk through the house, so behavioural live-fire is the USER's morning job — leave
  a checklist (below). You MAY: build, `cargo test`, deploy, confirm healthy startup,
  inspect case journals + logs, and (optionally) a no-arm `--once` on atlas.
- **HA help-channel:** the case journals live on atlas at
  `~/.local/share/argus/cases/<case_id>/` (events/*.json, stills/*.jpg).

## What the live test proved (don't regress these)

- ✅ Escalation ladder works: a confident intruder held `threat_present`, then a
  visible knife correctly escalated to `life_threatening` (the armed/weapon floor).
- ✅ Two-flow gating, persons model, threat rename all functioning.

## The four defects + their fixes

### 1. Two kinds of kiosk image — a priority-ranked MAIN image (broad) + concern-gated ID images

**Symptom:** no image on the HUD at all, and "never identified", in a real Alarm.
**Root cause (confirmed from the case record):** the present person was read as a
resident (`resident_confidence 0.75`, `intruder_confidence 0.2`), and BOTH still
capture AND the Opus ID are gated behind `Person::is_subject_of_concern`
(intruder-dominant). A resident-lean read suppressed the image entirely. The Opus ID
being skipped for a resident is actually CORRECT (see B); the real defect is that
there was no MAIN image to show. (Escalation still fired — the weapon/threat floor is
not concern-gated.)

There are **two image types on the kiosk, with DIFFERENT gating:**

**(A) MAIN image — the large primary pane. NOT concern-gated, PRIORITY-RANKED.**
The kiosk must never be blank when there's activity — show the best available still
*including an ambiguous/resident-only frame when that's all we have*. But prioritise,
and make rank **sticky-downward** (high→low):

| rank | category | when (this tick) |
|---|---|---|
| 3 | life_threatening | `threat_level == LifeThreatening` |
| 2 | malicious | `malicious_activity` non-empty |
| 1 | intruder | a person of concern present (`is_subject_of_concern`) |
| 0 | any | any person present at all |

Replacement rule: a newer still replaces the current main image **only when its rank
≥ the current main image's rank** — so newer-same-category replaces older, a higher
category replaces a lower one, and a lower category NEVER replaces a higher one
(e.g. newer malicious replaces older malicious, but does not replace a life_threatening
still; an ambiguous frame never displaces an intruder frame).

Implementation (engine, `run_tick`): reuse the frames ALREADY captured this tick (the
`stills: label→jpeg` map — no extra HA snapshot). Compute the tick's rank; pick the
camera showing the top-priority subject (a person of concern's `best_camera`/location;
else any person-present location). If `tick_rank >= main_still_rank`, `save_still` that
frame and set it as the main image. Add `CaseState.main_still: Option<StillRef>` (carry
its category/rank for the HUD label) + `ActiveCase.main_still_rank: u8` (reset per
case). Persist/broadcast as usual. HUD (`hud.js`): render `caseState.main_still` as the
primary pane (fallback to the existing `pickPrimaryView` heuristic if absent);
optionally label its category.

**(B) Intruder ID images — the small mugshot cards. CONCERN-GATED, once per new intruder.**
These are the Opus forensic pass + per-person `best_stills`. KEEP them gated to
**persons of concern** (intruders) — do NOT ID/card a resident or guest.
`upgrade_persons` stays concern-gated **as it is** (this was working as intended — the
test subject was a resident, so no ID is correct). HUD person cards stay
`subjectsOfConcern`-only. **The earlier idea of "show all persons as cards in Alarm"
is DROPPED** — only the MAIN image is broad; ID cards remain intruder-only. This is the
user's gating principle: "once we spot clear intruders, don't show where the residents
are; but don't hide ambiguous images when that's all we have."

**Verify Opus is healthy when it DOES run** (on a genuine intruder): the seed-only
system block + the `meaningful()` placeholder guard (0.33.x) are in place but were
never exercised (Opus was gated out last run). Confirm `apply_identification` yields
non-"placeholder" `descriptors`/`dossier`/`spoken_summary` and `IntruderIdentified`
fires once `ident.confidence >= ID_SPEAK_CONF` (0.5). If still junk, re-check
`run_identify` uses `self.seed` (not `self.system`).

**Acceptance:** with any person present the MAIN image is populated (ambiguous if
that's all); once an intruder/malicious/life_threatening still appears it takes AND
HOLDS the main pane over later ambiguous frames; ID mugshot cards appear only for
persons of concern, once per new ID, with real descriptors. Unit-test the
rank/replacement logic (pure function over (current_rank, tick_rank)).

### 2. Zone-movement on the kiosk ticker (stays concern-gated)

`IntruderEnteredZone` tracks a *person of concern* changing rooms and is a timeline
event the HUD ticker renders. In the test it didn't fire because the subject was a
resident — which is intended under the gating principle (don't narrate a resident's
movements). **Leave the concern-gating** (do NOT ungate zone tracking). Action:
VERIFY that for a genuine intruder the `IntruderEnteredZone` events fire and render in
the HUD ticker (audio is off, so the ticker is the only channel); consider surfacing
the intruder's current zone on the primary pane label. No new milestone.

### 3. HUD: radar animation → config option, OFF by default

The rotating radar sweep is too much. Make it **opt-in, default off**.
- `argus/web/hud.js` (`startRadar`): gate behind a top-of-file `const RADAR_ENABLED
  = false;` plus a URL-param override `?radar=1` for eyeballing. When off, don't
  start the RAF loop and hide/skip the `#radar` canvas (and drop the `--sweep-rate`
  driven cost).
- Keep the static chrome (tick rail, crosshairs, emblem). Acceptance: default HUD has
  no spinning sweep; `?radar=1` restores it.

### 4. HUD: distinguish Arming vs Armed, and threat_present vs life_threatening

- **Arming vs Armed look near-identical** (both steel "standby"). Give **arming** its
  own treatment: an **amber** palette and a "warning"-style centre animation (still
  animated in the same family as armed, but reads as caution). Armed stays steel/calm.
  - `argus/web/hud.js` `SYS` map: arming currently `state:"standby"`. Add a distinct
    `state:"arming"` (and set it for the `arming` mode) so CSS can theme it.
  - `argus/web/hud.css`: add a `[data-state="arming"]` theme (amber `--hud`/`--hud-rgb`)
    + a warning pulse on the centre element (the emblem / mid-screen circle). Keep
    `armed` as the calm steel standby.
- **Alarm urgency: threat_present vs life_threatening.** Keep the RED theme for the
  whole alarm state (don't change the base colour). Make **life_threatening** read as
  MORE urgent than threat_present — e.g. a faster/stronger border pulse AND an added
  pulsing element (headline or full-frame vignette pulse) only at life_threatening.
  The `THREAT` map in `hud.js` already differs per tier (`threat_present` rate 2.6 s /
  α0.20; `life_threatening` rate 1.2 s / α0.34) — amplify the life_threatening end and
  add the extra pulse cue; leave `benign`/`threat_present` calmer. The clearance label
  already shows THREAT vs LIFE THREAT.

### 5. Disarm latency — UI takes several seconds to reach AUTHORISED

**Root cause:** the engine's active-branch `select!` awaits a full `tick()` (the
~6–10 s multi-camera LLM round-trip) inside the timer arm, so an HA event (disarm)
that lands mid-tick isn't handled until the tick completes.
**Fix (`argus/src/engine.rs` `run()`):** let an incoming HA event **preempt** an
in-flight tick — race `self.tick()` against `rx.recv()` (and ideally `ctrl_rx`) so a
disarm is handled immediately; a preempted tick is simply dropped (its partial
assessment is discarded — acceptable; the next tick re-reads). Mind the borrow
checker (both borrow `self`/channels). If full preemption proves awkward, an
acceptable fallback is to broadcast the disarm `AlarmMode` + run `standdown()`
on the WS-event fast-path so the kiosks flip promptly even if the case flush lags.
**Acceptance:** kiosks reach the green AUTHORISED pane within ~1 s of a disarm.

### 6. Carried-over: revert the redundant automation edit (HA)

`Argus Trigger Reset` (id `argus_trigger_reset`) already pulse-resets both softer
booleans 2 s after they fire. The inline `delay`+`input_boolean.turn_off` I added to
**Visitor Approaching** (id `1781169387504`) is redundant — restore that automation's
actions to just `overwatch.play_tone double_beep` + `input_boolean.turn_on
input_boolean.argus_general` (drop the delay + turn_off), then `automation.reload`.
Leave the description's "Investigate" wording.

## Out of scope (user-owned — note, don't do)

- `escalation_alarm_script` (`script.argus_alarm` arming+triggering WITH the code) and
  `standdown_entity` (`input_button.argus_stand_down`) are still unset on atlas — they
  need the alarm code / the user's call. Leave a note; do not invent a code.

## Execution protocol (AUTONOMOUS — read first)

1. **Work on branch `argus-design-specs`.** Bump `argus/Cargo.toml` to **0.34.0**
   (new HUD config + behaviour). Keep `SCHEMA_VERSION` as-is (no journal shape change).
2. **Maximise sub-agents to preserve the main context.** Suggested split (sub-agents
   return concise summaries, not file dumps):
   - **Sub-agent A — Rust engine** (`src/engine.rs` + `src/case.rs`): item #1(A)
     (the MAIN-image capture + priority-rank + `CaseState.main_still`), item #5
     (disarm preempt). Items #1(B)/#2 are mostly "leave as-is + verify" — no engine
     change needed beyond confirming the concern-gate stays. One agent, sequential
     edits. Keep `cargo build --release` + `cargo test` green; unit-test the
     rank/replacement decision (pure fn over current_rank vs tick_rank).
   - **Sub-agent B — HUD frontend** (`web/hud.js`, `web/hud.css`, `web/demo.js`):
     item #1(A) HUD side (render `caseState.main_still` as the primary pane; cards
     stay `subjectsOfConcern`-only — do NOT show all persons), #3 (radar off), #4
     (arming amber + life_threatening urgency). Update `demo.js` to include a
     `main_still`. Separate files from A → safe in parallel. `node --check` each JS file.
   - **Main agent**: the HA automation revert (#6, quick API call), then serialise:
     `cargo build --release && cargo test`, `argus/deploy-container.sh`, confirm
     `Starting Argus v0.34.0` + HA auth in the journal. Then docs + ledger + commit.
   - Do NOT run two agents editing the SAME file concurrently.
3. **Validation you CAN do autonomously:** unit tests; clean build; deploy + healthy
   startup; inspect that the code paths are right; optionally a no-arm check. **Do NOT
   arm/trigger the real alarm.**
4. **Docs:** update `argus/CLAUDE.md` (the gating change + HUD config), this spec's
   status, `wiki/estate/shq.md` if hooks change, and append the outcome to ledger
   `shq-suite-0002`. Commit to `argus-design-specs` (do not merge to main); pushing the
   branch is fine.
5. **Leave a MORNING LIVE-TEST CHECKLIST** for the user (re-run Test 1 Alarm: expect
   image on HUD, an Opus ID line, zone-movement lines on the kiosk ticker; check
   arming=amber vs armed=steel; threat_present vs life_threatening urgency; radar
   off by default; disarm → AUTHORISED within ~1 s; then Tests 2 & 3 Investigate).

## Verification checklist (agent, before declaring done)

- [x] `cargo build --release` clean (no new warnings in argus crate).
- [x] `cargo test --release` all green (42 passed; incl. the new
      `should_replace_main` / `compute_main_rank` rank tests).
- [x] `node --check web/hud.js && node --check web/demo.js` (both OK).
- [x] Deployed; journal shows `Starting Argus v0.34.0`, HA WebSocket authenticated,
      `+ 2 softer trigger(s)`.
- [x] Visitor Approaching automation (id `1781169387504`) reverted to
      `play_tone double_beep` + `turn_on argus_general` only; `automation.reload` ok.
- [x] Docs + ledger updated; committed to `argus-design-specs`.
- [x] Morning checklist left for the user (below).

## 🌅 MORNING LIVE-TEST CHECKLIST — ✅ COMPLETED (superseded by live-fire runs)

> This was the pre-live-fire checklist for 0.34.0. It has since been **completed and
> far exceeded** by a series of full live-fire walk-throughs (Overwatch voice + klaxon
> and PagerDuty live, NOT muted) that drove the work to 0.36.0 — see the status block
> at the top of this file and ledger `shq-suite-0002`. Kept below for historical record.

Test posture for the night was unchanged: **Overwatch amp OFF** (no audible
TTS/klaxon), **PagerDuty in maintenance**. Re-check both before/after as you wish.
Watch the HUD on **kiosk11** and the case journal on atlas
(`~/.local/share/argus/cases/<case_id>/`; live logs:
`ssh jordonsc@atlas.shq.sh 'journalctl --user -u argus.service -f'`).

**Test 1 — Alarm (arm + walk through, as last time):**
- [ ] **HUD is NOT blank** — the primary pane shows a MAIN image as soon as anyone
      is present (even an ambiguous/resident-only frame). #1A
- [ ] As you escalate (intruder / a held weapon), the image **upgrades and HOLDS**
      the higher-priority frame; an ambiguous frame later does NOT displace it. #1A
- [ ] An **Opus ID line** appears for a genuine intruder (real descriptors, not
      "placeholder"), and an **intruder ID mugshot card** shows — once per new ID,
      intruder-only (a resident is NOT carded). #1A/#1B
- [ ] **Zone-movement lines** ("Intruder in <room>.") render in the kiosk ticker as
      you move room-to-room (audio is off, so the ticker is the only channel). #2
- [ ] **Disarm → green AUTHORISED pane within ~1 s** (not the old several-second
      lag), even if a tick was mid-flight. #5
- [ ] **Radar sweep is OFF** by default (static chrome only); append `?radar=1` to
      the kiosk URL to confirm it still works on demand. #3
- [ ] **Arming reads amber/caution**, **armed reads calm steel** (visibly
      different now). #4a
- [ ] **`life_threatening` is visibly MORE urgent** than `threat_present` (faster
      border pulse + a full-frame vignette/headline pulse only at life_threatening),
      both still on the RED alarm theme. #4b

**Tests 2 & 3 — Investigate (front-door approach, gated):** re-run the gated
double-check (a benign approach should stay silent + quietly stand down; a genuine
armed/forced-entry approach should escalate). Confirm the gating still behaves.

**Still UNSET (user-owned, see Out of scope):** `escalation_alarm_script`
(`script.argus_alarm`) and `standdown_entity` (`input_button.argus_stand_down`) — they
need the real alarm code / your call. Until set, an escalation falls back to the
direct `alarm_trigger`, and the kill-switch standdown entity is absent.
