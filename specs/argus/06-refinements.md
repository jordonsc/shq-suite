# Phase 6 — Post-M1 Refinements & Live-Testing Log

> Sub-spec of [Argus master plan](./00-master.md). **Status: 🔧 ACTIVE — live-fire
> validated end-to-end (2026-06-19).** This is the **resume point** after a long
> live-testing + hardening session. Argus is at **0.19.0**; Overwatch at **0.3.0**.
> The whole pipeline (real alarm → location-aware voice breach → per-camera
> assessment → weapon detection → forensic ID → sequential speech → standdown) has
> been validated live against the **real** Overwatch + alarm. See ledger
> `shq-suite-0002`. Argus is now at **0.24.0**, **containerised on atlas** (rootless
> Podman + systemd Quadlet, ledger `shq-suite-0004`), with the **kiosk HUD takeover
> LIVE-FIRED on kiosk11** (alarm-mode arming/armed/triggered/authorised panes + 15s
> green dwell) and the **disarm logic moved off HA into Argus** — see the version
> history below.

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
5. **Overwatch voice is MUTED on atlas (session of 2026-06-19 PM):** the
   `~/.config/argus/config.yaml` `overwatch:` key was renamed (→ no voice channel
   spawns) so trigger-testing stays silent (people sleeping). Restore from
   `~/.config/argus/config.yaml.voicebak`, or rename the key back, + restart. **While
   muted the Argus disarm voice + klaxon-off are DORMANT** (the outputs consumer isn't
   spawned) — verify them once voice is back.
6. **HA "Alarm Disarmed" automation** had its `overwatch.verbalise "Security
   disarmed"` action removed (now Argus's job). Backup: `/tmp/alarm_disarmed.bak.json`.
   The redundant HA klaxon-off (`script.alarm_stand_down`) is intentionally LEFT as a
   safety net until Argus's klaxon control is live-verified.

**Argus now runs CONTAINERISED on atlas** (systemd `--user` Quadlet, `argus.service`)
watching `alarm_control_panel.shq_alarm` — the old Valerie shell daemon was retired.
**kiosk11** (720×1280 portrait, `idle_mode: off`, registered in HA `shq_display`) is
the live takeover test subject.

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
- **0.20.0 — intruder-movement zone announcements.** A terse "Intruder in
  `<zone>`." is spoken the first time an intruder is seen in a fresh room/area. A
  *zone* is a new optional `CameraConfig.zone` (defaults to the camera `label`), so
  cameras can share one — the two `Outdoor Living` cameras both map to `Backyard`
  (the only real overlap; everything else is 1:1). `merge_live →
  announce_zone_entries` derives the milestone (`TimelineKind::IntruderEnteredZone`)
  from each intruder's location; dedup is **zone-level**
  (`ActiveCase.announced_zones`). The **initial trigger zone is excluded** — seeded
  as already-announced at case open from `trigger_location` (compared
  case-insensitively, so zone names must match the HA area names
  `input_text.alarm_trigger_room` reports); if no trigger location is known the
  first observed zone is suppressed as the de-facto initial. Voice-only (no extra
  PagerDuty page). The live config on atlas needs the `Backyard` zone added to the
  two outdoor-living cameras for the overlap-collapse to take effect.
- **CONTAINERISED + systemd on atlas (ledger `shq-suite-0004`).** Argus now runs ON
  atlas as a rootless Podman container via a systemd `--user` Quadlet unit
  (`argus.container`), mirroring atlas's qdrant/rag-serve. Multi-stage `Containerfile`
  (~109 MB), `Network=host`, config/seed/secrets + case journal bind-mounted
  (`HOME=/root` ⇒ byte-identical to native), `argus.env` EnvironmentFile, boot-
  persistent via linger. Deploy/redeploy: `argus/deploy-container.sh`. The old
  Valerie shell daemon was stopped (cutover). Test posture on atlas: `klaxon_enabled:
  false`, `voice_volume: 0.4` (voice level 4).
- **0.21.0 — live-test refinements (movement latency + identify-once).** First live
  test of the containerised stack succeeded; two quirks fixed: (1) **room-change
  announcements lagged ~20 s** because they keyed off the intruder's reconciled
  best-view location — now they fire off the **per-camera sighting** for the tick
  (`zones_seen`, before the merge), and while `actively_tracking` (intruder on roster
  + recent activity) the loop forces a **full sweep every tick** so a newly-entered
  room isn't motion-gated out. Floor is now ~one cadence + LLM (~6–11 s); sensor
  propagation is the irreducible part. (2) **Identification was verbalised twice** —
  the Opus pass re-runs as stills improve and each completion re-emitted
  `IntruderIdentified`. Now edge-triggered: spoken profile + PD page fire ONCE per
  intruder; later passes firm up descriptors silently.
- **0.22.0 — quality-aware forensic frame selection (snappier + better ID).** The
  live Sonnet output now scores each frame's fitness for identification
  (`id_quality`, 0–1, distinct from `confidence`). `merge_intruders` points
  `best_camera` at the highest-`id_quality` view so the Opus pass profiles on the
  CLEAREST frame (a clearer frame is also faster + more confident for Opus); the
  re-profile trigger switched from "+0.1 confidence" to "id_quality beats the last
  profiled frame by `QUALITY_IMPROVE` (0.12)" — chasing a better picture. The spoken
  identification is additionally gated on `confidence >= ID_SPEAK_CONF` (0.5) so the
  announced profile is solid, not a first-glimpse guess (sub-threshold passes still
  fill the record/dossier silently). Opus always profiles at least once on first
  detection — quality steers WHICH frame + WHEN to re-run, it never withholds the
  pass.
- **0.22.2 — `ID_SPEAK_CONF` validated at 0.5.** From the live journal a real
  intruder sits ~0.9+ while a distant passer-by/false figure sits ~0.3, so 0.5
  cleanly separates them (a 0.4 build was tried + reverted). Comment records the why.
- **HUD redesign (frontend, `web/`).** Live-tuned on kiosk11 (a real 720×1280
  PORTRAIT kiosk — the HUD is portrait-first): SHQ wordmark (was BLACKROSE), local
  clock in all modes, removed top/bottom crosshair circles, ~3× camera caption,
  landscape subject panels, big single-line situation feed packed top (`align-content:
  start`), radar trail now FOLLOWS the sweep line, threat badge clears the corner
  brackets. The feed "status" is a short controlled enum (`feedStatus()` —
  `Intrusion in progress`/`All clear`, extensible for trigger profiles) not the
  free-text LLM summary (which overflowed). Flexbox `min-width:0` fixes fixed the
  right-edge overflow. **Main pane is intruder-centric** (`pickPrimaryView`): it
  follows the latest intruder (location/activity/best still), never a stray camera
  note (e.g. the garage's parked car). Web is now served from a **bind-mount**
  (`~/.config/argus/web`) so frontend tweaks are an rsync + reload, no image rebuild.
- **0.23.0 — alarm-mode takeover + standby panes (Phase 4 LIVE-FIRED).** Argus now
  watches the WHOLE alarm state machine, not just `triggered`. New `state::AlarmMode`
  {Disarmed, Arming, Armed, Triggered}; the HA WS emits `AlarmModeChanged`; the engine
  broadcasts the mode on a new `watch<AlarmMode>` channel + opens/closes the case as
  before. The kiosk-takeover consumer + the `/kiosk` WS now consume BOTH the case and
  the mode: **arming/armed flip the kiosk to the HUD standby pane** (a `{type:"system",
  mode}` WS frame the HUD renders), `triggered` shows the alarm pane, all on ONE
  `/alarm` URL (no reload between states). **Live-fired on kiosk11** (idle_mode off →
  no nyx/Chronos blocker): arm → ARMING → SYSTEM ARMED → (trigger) alarm pane →
  (disarm) revert, all validated against the real alarm. Engine reads the alarm state
  once at startup so an already-armed alarm shows immediately.
- **0.23.1 — 15s AUTHORISED dwell on every disarm.** On disarm the HUD holds a green
  "AUTHORISED / all clear" for 15s, then reverts to dashboard. Post-incident shows the
  green cleared `CaseState`; a no-incident disarm (armed→disarmed) broadcasts a
  transient `AlarmMode::Authorised` → a green "ACCESS GRANTED" system pane. Engine
  `revert_pending: (Instant, Revert)` services both (ClearCase → broadcast `None`;
  ToDisarmed → broadcast `Disarmed`); a fresh case/arm cancels it. Verified live: the
  green held exactly 15.0s (journal 08:19:50 clear → 08:20:05 revert).
- **0.24.0 — disarm voice moved into Argus + main-pane intruder fix.** Argus took the
  disarm logic off HA: on the active→disarmed mode edge the outputs consumer (a) stops
  the klaxon **out-of-band** (direct `SetAlarm(false)`, not behind the serial speech
  worker — fixes the long-noted klaxon-stop-waits-behind-a-line issue), (b) **flushes
  queued intruder lines** via a generation counter (`VoiceMsg::Speak{generation}`; the
  worker skips stale lines — the in-flight clip can't be stopped, Overwatch has no
  interrupt, but everything behind it is dropped), (c) announces **"Alarm standing
  down."** The HA "Alarm Disarmed" automation had its `overwatch.verbalise "Security
  disarmed"` action removed (DOSA-restore + kiosk02–10 dashboard kept; backup at
  `/tmp/alarm_disarmed.bak.json`). The redundant HA klaxon-off (`script.alarm_stand_down`)
  is LEFT as a safety net until Argus's klaxon control is live-verified. Plus the
  main-pane intruder-centric fix (see HUD redesign above).

## Kiosk 02–10 cutover runbook (Argus takeover migration)

> **Scaffolding + runbook DONE** (config-example entries added, live HA path
> enumerated read-only). **The live cutover is a MORNING task with the user
> present** — kiosks 02–10 are in active household use, so do this at a considered
> time, not blind. Nothing below was actioned during the overnight scaffolding run.

### What the legacy HA path does today (enumerated read-only, 2026-06-19)

kiosk11 already runs on Argus. kiosks 02–10 are still driven by **two HA scripts**
called from the alarm-state automations (NOT the trigger automation — takeover
happens at **armed**, restore at **disarmed**):

| Script | Navigates kiosk02–10 to | Called from automation |
|--------|-------------------------|------------------------|
| `script.kiosks_alarm` ("Kiosks - Alarm") | `http://atlas.shq.sh:8123/dashboard-kiosks/alarm?kiosk` | **Alarm Armed** (id `1770878937433`), on `arming → armed_away` |
| `script.kiosks_dashboard` ("Kiosks - Dashboard") | each kiosk's own `…/dashboard-kiosks/kioskNN?kiosk` | **Alarm Disarmed** (id `1770879164083`), on `→ disarmed` |

Each script is a single `parallel:` block of nine `shq_display.navigate` calls
(`device_id: kiosk02 … kiosk10`). The dashboard-restore URL pattern matches the
Argus `dashboard_url` exactly (`…/dashboard-kiosks/kioskNN?kiosk`), so the
config-example entries are 1:1 with the live restore targets.

**Important — the kiosk-restore is embedded in a multi-purpose automation.** The
**Alarm Disarmed** automation (id `1770879164083`) also runs the DOSA-sensor
restore (`input_boolean.laundry_door_auto` if it was on pre-arm). The **Alarm
Armed** automation (id `1770878937433`) also caches/disables DOSA sensors, turns
off `input_boolean.perimeter_security`, and speaks the "Security is now armed"
line. So **do NOT disable these whole automations** — only neutralise the kiosk
calls. The clean cut is to **remove the `script.kiosks_alarm` / `script.kiosks_dashboard`
action from each automation** (or empty the two scripts' sequences), leaving the
DOSA/perimeter/voice logic intact. (Disabling the two *scripts* outright also works
and is fully reversible, but a disabled script called from a running automation
logs a warning each disarm — removing the call is tidier.)

Other alarm automations (NOT touched by this cutover): `Alarm Sensors` (id
`1770875325186`, sets `input_text.alarm_trigger_room` + trips the panel),
`Alarm Triggered` (id `1770876717605`, currently DISABLED per the test posture),
`Stand Down Alarm` (id `1770878315171`, `script.alarm_stand_down` klaxon-off
safety net), `Alarm Arming` (id `1770878814394`, arming voice).

### Cutover steps (do in this dependency order, with the user)

1. **Reflash nyx 1.2.0 onto kiosks 02–10** (they run 1.1.0; the wake/keep_awake
   fields are no-ops on older nyx, so the takeover would be invisible behind a
   clock/blank screensaver). Also ensure the `shq_display` HA component is ≥ 1.2.0
   (load via `./setup ha --restart` if not already done for kiosk11):
   ```bash
   cd nyx && ./build-rpi.sh
   cd .. && ./setup nyx          # deploys to all kiosk hosts (or per-host if the tool scopes)
   ```
   Verify each kiosk reports nyx 1.2.0 before proceeding.

2. **Add kiosks 02–10 to the LIVE Argus config** — edit `~/.config/argus/config.yaml`
   on atlas (gitignored; mirror the change to shq-suite-config afterwards). Append
   to the `kiosks:` block (kiosk11 is already there):
   ```yaml
   kiosks:
     - { ha_target: "kiosk02", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk02?kiosk" }  # Garage
     - { ha_target: "kiosk03", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk03?kiosk" }  # Kitchen
     - { ha_target: "kiosk04", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk04?kiosk" }  # Jordon Study
     - { ha_target: "kiosk05", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk05?kiosk" }  # Laundry / DOSA
     - { ha_target: "kiosk06", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk06?kiosk" }  # Entrance
     - { ha_target: "kiosk07", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk07?kiosk" }  # Bed 1
     - { ha_target: "kiosk08", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk08?kiosk" }  # Gym
     - { ha_target: "kiosk09", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk09?kiosk" }  # Sal Study
     - { ha_target: "kiosk10", dashboard_url: "http://atlas.shq.sh:8123/dashboard-kiosks/kiosk10?kiosk" }  # Dining Room
     # kiosk11 already present (test unit)
   ```
   (`web:` must already be configured for takeover to run — it is, kiosk11 is live.)

3. **Redeploy + restart Argus** to pick up the new kiosk list:
   ```bash
   argus/deploy-container.sh
   ssh atlas 'systemctl --user restart argus.service'
   ssh atlas 'journalctl --user -u argus.service -f'   # watch for the takeover navigates
   ```
   (A config-only change still needs the container restart — the config is
   bind-mounted read-only and read once at startup.)

4. **Retire the HA-driven kiosk path** — once Argus is verified driving 02–10,
   remove the kiosk action from each automation (NOT the whole automation):
   - **Alarm Armed** (id `1770878937433`): remove the `script.kiosks_alarm` action.
   - **Alarm Disarmed** (id `1770879164083`): remove the `script.kiosks_dashboard` action.

   **Prefer reversibility: disable (don't delete).** The lowest-risk form is to
   *empty the two scripts' sequences* (or disable the two scripts) so the automation
   calls become no-ops but every entity/id is preserved for rollback. Back up both
   automation + script configs first (e.g. `./ha get /api/config/automation/config/1770878937433`,
   same for `…/1770879164083` and `/api/config/script/config/kiosks_alarm`,
   `…/kiosks_dashboard`) — mirror the originals to shq-suite-config.

### Per-kiosk verification (repeat for each of 02–10)

With the user, run the real alarm machine and confirm on each kiosk:

- **Arm** → after wake, the kiosk shows the **HUD standby pane** (steel "SYSTEM
  ARMED"). On a kiosk that was on a clock/blank screensaver, confirm it **woke**
  (proves nyx 1.2.0 + `wake:true` landed).
- **Trigger** (arm → walk past a camera) → the kiosk flips to the **alarm pane**
  (takeover) and the screen is **force-on** (`keep_awake` pins it — it must not
  blank/clock during the incident).
- **Disarm** → the kiosk holds the green **AUTHORISED** dwell (~15 s) then is
  **restored to its own dashboard** (`…/dashboard-kiosks/kioskNN?kiosk`), and
  **normal idle blank/clock resumes** afterwards (the `keep_awake` pin released).

Watch the Argus journal for one `shq_display.navigate` per kiosk on each edge, and
confirm the legacy HA scripts no longer fire (no duplicate navigate).

### Rollback

If Argus's takeover misbehaves on 02–10:
1. **Re-enable the HA path** — restore the `script.kiosks_alarm` / `script.kiosks_dashboard`
   actions in the two automations (or re-enable/repopulate the scripts) from the
   backups taken in step 4.
2. **Remove kiosks 02–10 from `~/.config/argus/config.yaml`** (leave kiosk11),
   then `argus/deploy-container.sh` + `systemctl --user restart argus.service`.

The two halves are independent and either order is safe; doing both fully reverts
to the pre-cutover behaviour. (nyx 1.2.0 on the kiosks is harmless to leave — the
wake fields are simply unused by the HA path.)

### Open items to confirm at cutover

- **Legacy takeover is at `armed`, Argus's is at `arming`+`armed` (standby pane)
  then `triggered` (alarm pane).** Argus shows a richer arming→armed→triggered
  sequence on ONE `/alarm` URL; the legacy path only flips to a static alarm
  dashboard at armed. Confirm the user is happy the standby pane replaces the
  current armed-dashboard behaviour on 02–10.
- **Does `./setup nyx` fan out to all kiosk hosts, or is it per-host?** Confirm the
  deploy tool's kiosk scoping before the reflash so all of 02–10 get 1.2.0.
- The `shq_display` HA component version on atlas — confirm it is ≥ 1.2.0 (needed
  for the navigate `wake`/`keep_awake` fields) before relying on wake-through-clock.

## Open findings / next steps (for the fresh-context continuation)

> The **canonical milestone roadmap** is
> [`00-master.md` § Milestone task lists](./00-master.md#milestone-task-lists). The notes
> below are the design rationale behind those tasks (milestone tags in brackets), plus the
> findings already resolved by the live-hardening pass.

- **Resident reference photos** [M1] (was refinement #2) — attach resident photos to the
  Opus identify (and maybe the live) call to anchor recognition. Store privately
  (shq-suite-config / not the public repo). The richer `authorised: bool`-per-person model
  (vs the current exclude-from-intruders approach) is the related `CaseState` schema
  option. Design + build pending.
- **Trigger PROFILES `[Alarm, Investigate, General]`** [M1] — **PROMOTED TO M1**
  (2026-06-19) and expanded from two types to three; full design in
  [`07-trigger-profiles.md`](./07-trigger-profiles.md). `Alarm` = current path
  (intrusion assumed, outputs immediate); `Investigate` = perimeter security smart
  pre-alarm (are residents in danger / only non-residents? outputs GATED until Argus
  self-escalates); `General` = benign trigger e.g. front-door approach (fast, shallow
  check for obvious threats — balaclava/weapon — then escalate or stand down quickly).
  The profile is carried into the case (`CaseState.trigger_profile`) and gates the
  outputs for the two softer profiles until promotion. Pending build.
- **Klaxon volume during speech** [M1, Overwatch] — the klaxon-**stop** is now ✅ resolved
  (0.24.0 stops it out-of-band, off the serial speech worker, so disarm silences it
  immediately). The remaining task is **ducking the klaxon volume while a line plays** so
  the verbalised lines are intelligible over it.
- **Weapon perception is still the ceiling** — per-camera fan-out + hard prompts got
  it firing reliably (e.g. "possible knife"), but a small blade at camera distance
  can still read as a phone/LEGO. Resident photos [M1] + a possible weapon-focused crop
  pass (future lever) are the levers.
- **Production model decision** [M1 open question] (see TEST POSTURE above): Argus replacing
  vs augmenting the legacy `script.alarm_trigger_actions`.

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
