# Phase 6 — Post-M1 Refinements & Live-Testing Log

> Sub-spec of [Argus master plan](./00-master.md). **Status: 🔧 ACTIVE — live-fire
> validated end-to-end (2026-06-19).** This is the **resume point** after a long
> live-testing + hardening session. Argus is at **0.19.0**; Overwatch at **0.3.0**.
> The whole pipeline (real alarm → location-aware voice breach → per-camera
> assessment → weapon detection → forensic ID → sequential speech → standdown) has
> been validated live against the **real** Overwatch + alarm. See ledger
> `shq-suite-0002`.

## ⚠️ CURRENT HOUSE POSTURE — TEST MODE (left deliberately, must restore later)

The premises were left in a **degraded test posture** at the user's request so
development can continue. **A real intrusion right now gets a reduced response.**
To restore production:

1. **Re-enable** `automation.alarm_trigger_2` ("Alarm Triggered") — currently
   **DISABLED** (so the legacy klaxon + PagerDuty page do NOT fire on a real
   trigger; Argus was taking over those outputs). `automation.alarm_trigger`
   ("Alarm Sensors", sets `input_text.alarm_trigger_room` + triggers the panel) and
   `stand_down_alarm` are still enabled.
2. **Flip `~/.config/argus/config.yaml` `overwatch:` back to production:**
   `klaxon_enabled: true`, `klaxon_volume: 0.9`, `voice_volume: 1.0`
   (currently `false` / `0.1` / `0.6` for dog-friendly low-volume testing).
3. **HA `arming_time`** was set to **5s** (was 3 min) in
   `deploy/config/ha/configuration.yaml` for fast test re-arms — restore if desired
   (deployed via `./setup ha --restart`).
4. Decide the **production model**: does Argus *replace* the legacy
   `script.alarm_trigger_actions` voice/klaxon/PD, or run alongside it? (We disabled
   the legacy output to avoid a double klaxon; the lighting scene + legacy PD also
   went with it.) This is a design decision for M2.

A live Argus daemon (0.19.0, voice-only) is currently **running** on atlas under
the user shell (not yet a systemd service) watching `alarm_control_panel.shq_alarm`.

## Live-testing harness

- **Scratch alarm (non-disruptive):** point `alarm_entity:
  "sensor.argus_test_alarm"` (a synthetic HA state entity). Drive with
  `POST /api/states/sensor.argus_test_alarm {"state":"triggered"|"disarmed"}`.
  Used for the dry-voice runs.
- **`--dry-voice`** (0.8.0): runs the full Phase-3 outputs consumer but LOGS
  `[DRY VOICE] would speak/SetAlarm` instead of dialling Overwatch. Safe with no
  klaxon/TTS.
- **Real live-fire:** the daemon watches the real `alarm_control_panel.shq_alarm`;
  the user arms (now 5s) + walks past a camera → "Alarm Sensors" sets the trigger
  room + fires the panel → Argus opens a case and drives real Overwatch at the
  test volumes. **Klaxon is currently disabled** (`klaxon_enabled: false`) so it
  doesn't distress the dog (Ubu) — that's why testing is voice-only.
- Pull secrets non-interactively:
  `export ANTHROPIC_API_KEY=$(bash -ic 'printf %s "$ANTHROPIC_API_KEY"' 2>/dev/null)`
  (same for `HA_URL`/`HA_TOKEN`).
- **Default posture = intrusion in progress** (real runs are alarm-initiated).

## What the BEST run validated (2026-06-19, 0.19.0, real Overwatch, voice-only)

Real arm → walk into kitchen with a knife. Spoken, in order, no overlap:
1. *"Security breach detected in Kitchen."* (~3s after trigger; real trigger room)
2. *"Weapon detected! Intruder 1 armed with possible knife."* (~26s)
3. *"Intruder identified. Caucasian adult, slim build, navy t-shirt, grey shorts,
   reddish-blonde hair, holding a blade at the kitchen bench."* (~54s)
4. *"Intruder profile sent to security responders."*
Final state: `armed=true weapon="possible knife" identified=true threat=critical`,
clean standdown on disarm. Threat **held Critical** the whole time (ratchet).

## Version history this session (argus 0.7.0 → 0.19.0; Overwatch 0.3.0)

- **0.7.0 — motion-gated ticks.** Baseline full sweep, then only cameras with
  recent smart-detect activity (+ any tracking an intruder); skip the LLM when
  idle; periodic full re-sweep (`full_sweep_every`).
- **0.8.0 — weapons + resident-certainty + dry-voice.** `Intruder.{armed,weapon}` +
  case `threats` + `WeaponDetected` milestone; armed LATCHES on; resident
  recognition requires beyond-reasonable-doubt (fail toward intruder); `--dry-voice`.
- **0.9.0 — concurrent forensics + voice overhaul.** Opus identify runs in a
  spawned task (results posted back over an mpsc, applied on the engine task — no
  locking); the live loop no longer blocks on Opus. Voice gate restructured:
  CaseOpened → breach line; IntruderIdentified → profile + sent line; WeaponDetected
  → weapon line; threat-level/best-still now silent.
- **0.10.0 — threat ratchet + location + volume split.** Threat NEVER downgrades
  from the model mid-incident; only inactivity decays it (180s→Elevated,
  600s→Info/low) and **manual disarm or 1h-no-activity** closes the case. Breach
  line names the trigger room from `input_text.alarm_trigger_room`
  (`trigger_location_entity`). `klaxon_volume`/`voice_volume` split.
- **0.11.0 — `klaxon_enabled` toggle** (voice-only mode; the dog-friendly default
  for testing).
- **0.12.0 — concise spoken profile + paced speech.** Forensic pass writes a short
  `spoken_summary` (the FULL descriptors stay on the record/PD); voice worker added
  a duration estimate between lines.
- **0.13.0 — best-camera fallback** for the forensic still.
- **0.14.0 / Overwatch 0.3.0 — BLOCKING verbalise.** Overwatch `VerbaliseRequest`
  gained optional `await_playback`; when true, playback uses `sink.sleep_until_end()`
  so the RPC returns only after the clip FINISHES. Argus sets it → the serial voice
  worker paces on real playback (no timing guess); the 0.12.0 estimate-sleep removed.
  Default false → HA's existing TTS path unchanged. **Overwatch redeployed** to the
  voice RPi (ARM64). Tradeoff: a blocking clip defers other audio commands (e.g.
  klaxon-stop) by up to its length — an out-of-band klaxon-stop is a noted follow-up.
- **0.15.0 — entity-id ↔ label normalisation (latency fix).** The model returns
  camera ENTITY IDs (they're in the seed's camera map) where labels were expected;
  stills are keyed by label, so still-lookups missed and the forensic pass only
  started once motion-gating narrowed to one camera (~69s). Now normalised
  everywhere → the pass fires on first detection.
- **0.16.0 — harder weapon prompts** on BOTH models: scrutinise hands/held objects,
  flag `armed` for anything plausibly a weapon (lean toward flagging during an
  active intrusion), hedge in `weapon` ("possible knife"); only a clearly-benign
  object (phone/remote/cup) is excluded.
- **0.17.0 — per-camera fan-out (the big weapon-detection win).** The live loop
  sends ONE focused Sonnet call PER active camera, concurrently (was one diluted
  multi-image call), then merges: intruders grouped by id (armed=OR, weapon prefers
  an armed sighting, confidence=max, descriptors from the best frame), threat=max,
  locations concatenated, threats deduped. Full-res, full-attention per frame.
- **0.18.0 — fixed-rate frame pacing.** Loop targets a ~6s frame PERIOD (pads only
  if the per-camera LLM calls finished faster; a slow tick fires the next promptly)
  instead of adding 6s on top of each tick. ~6s between frames for the tracked
  camera (was ~12s).
- **0.19.0 — activity-adaptive cadence.** Once the case is quiet ≥180s (decayed off
  Critical), the loop eases to `slow_cadence_secs` (30s); fresh activity snaps it
  back to 6s.

## Open findings / next steps (for the fresh-context continuation)

- **Resident reference photos** (was refinement #2) — attach resident photos to the
  Opus identify (and maybe the live) call to anchor recognition. Store privately
  (shq-suite-config / not the public repo). Design + build pending.
- **Trigger TYPES `[alert, investigate]`** (was refinement #3, pulls parked M2
  perimeter-security forward): `alert` = current path; `investigate` = analyse then
  optionally self-escalate (person on external cams while the house is inactive,
  `timer.perimeter_security_cooldown`). A trigger-type carried into the case; in
  `investigate`, outputs are gated until Argus concludes a real threat.
- **Out-of-band klaxon-stop** — with blocking verbalise, a standdown's klaxon-off
  can wait behind a playing line (≤ its length). For real-klaxon use, route the
  klaxon-stop off the serial speech worker so disarm silences it immediately.
- **Weapon perception is still the ceiling** — per-camera fan-out + hard prompts got
  it firing reliably (e.g. "possible knife"), but a small blade at camera distance
  can still read as a phone/LEGO. Resident photos + a possible weapon-focused crop
  pass are future levers.
- **Production model decision** (see TEST POSTURE above): Argus replacing vs
  augmenting the legacy `script.alarm_trigger_actions`.
- `authorised: bool`-per-person richer model (vs the current exclude-from-intruders
  approach) remains a noted `CaseState` schema option.

## Housekeeping

- **Committed this session** (no push, branch `argus-design-specs`): argus
  0.7.0→0.19.0 + Overwatch 0.3.0 + docs. The private `~/.config/argus/{config.yaml,
  seed.md}` are gitignored (live on atlas only).
- **OWED:** mirror the seed change (resident near-certainty + weapon clauses) and
  the config to the private `shq-suite-config`. Restore production posture (above)
  when testing is done. Re-enable the stale `unifiprotect` entry cleanup is
  cosmetic (`shq-suite-0003`).

## Motion-sensor finding → ledger `shq-suite-0003`

On this NVR (UniFi Protect 7.1.77) the **8× G6 PTZ cameras are smart-detect-only**
(`smartDetectZone`, not base `EventType.MOTION`), so HA's `binary_sensor.<cam>_motion`
never fires for them (HA issue #124967). Only the 2 fixed G5 Domes (garage, laundry)
emit real motion. **Gate on smart-detect sensors** (`_person/_vehicle/_animal_detected`
+ `_motion` for garage/laundry), OR'd — the config + `config.yaml.example` list all four.
