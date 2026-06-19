# Argus

AI-powered alarm assessment daemon. When `alarm_control_panel.shq_alarm`
transitions to `triggered`, Argus captures camera stills, sends them to Claude
with the private premises seed, and logs a natural-language assessment.

**Native x86_64** service that runs on **atlas** (the HA + RAG host) — `cargo
build --release` + systemd, **not** a `cross`/Podman RPi build like nyx/overwatch/dosa.

> **Phase status:** Phases 1 (foundation), 2 (assessment loop + `CaseState`), 2a
> (offsite S3 resilience), 3 (outputs — Overwatch voice + PagerDuty), and 4
> (kiosk HUD server + takeover — Rust side) are implemented + shape-verified.
> **Phase 5 (control WS — Rust side)** is implemented: the `/control` WebSocket on
> the HUD server + the `ControlCommand` channel into the engine (the HA `argus`
> component connects here). The HA component itself + deploy + the real seed are
> owned by other Phase 5 work. See `specs/argus/` (read
> `00-master.md` first). **Phase 3 voice IS now live-fired** (2026-06-19): the full
> pipeline (real alarm → location-aware breach line → per-camera assessment →
> weapon detection → forensic ID → sequential speech via blocking `Verbalise`) has
> been validated against the **real** Overwatch. The klaxon path exists but is run
> with `klaxon_enabled: false` during testing (pets). PagerDuty is still
> shape-only (no routing key). **Phase 4 takeover IS now LIVE-FIRED** (2026-06-19) on
> **kiosk11** (720×1280 portrait, `idle_mode: off`): the alarm-mode takeover drives
> arm → ARMING → SYSTEM ARMED → (trigger) alarm pane → (disarm) green AUTHORISED 15s →
> dashboard, validated against the real alarm. The `web/` HUD app (frontend-owned) was
> redesigned + tuned on the real kiosk. **A large post-M1 live-hardening
> pass (argus 0.7.0→0.24.0 + Overwatch 0.3.0 blocking verbalise, now CONTAINERISED on
> atlas) is captured in
> [`specs/argus/06-refinements.md`](../specs/argus/06-refinements.md) — read it
> first for the current state + the TEST-POSTURE (incl. Overwatch voice MUTED for the
> session).** **The remaining roadmap (M1/M2/M3) is the structured task list in
> [`specs/argus/00-master.md` § Milestone task lists](../specs/argus/00-master.md#milestone-task-lists).**
> **Next M1 build: trigger profiles** (`Alarm`/`Investigate`/`General` tiered response) —
> design complete in
> [`specs/argus/07-trigger-profiles.md`](../specs/argus/07-trigger-profiles.md),
> not yet implemented.

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
| `src/main.rs` | Entry point + CLI. Loads config/seed; builds the Sonnet + Opus clients + the `watch<Option<CaseState>>` channel; `--once` (single tick → print CaseState), `--dry-voice` (run the full voice gate but LOG lines/klaxon instead of contacting Overwatch — forces the outputs consumer on, overrides `overwatch:`), and the daemon (Ctrl-C aborts the engine task) |
| `src/config.rs` | `config.yaml` loader. `${VAR}` env expansion, `~` expansion, default path `~/.config/argus/config.yaml`. Adds `loop_config` (cadence/cap) + `telemetry_entities` + `offsite` (Phase 2a S3 replication; `OffsiteConfig`/`UploadConfig`). `CameraConfig.zone` (optional; room/area for movement announcements, defaults to `label`; cameras sharing a zone announce once) |
| `src/case.rs` | **The `CaseState` contract** + `LiveAssessment`/`Identification` LLM-output types, their `*_schema()` JSON schemas, `TimelineKind` vocabulary, and `CaseDir` (the on-disk journal = Phase 2a upload queue). Carries **weapon/threat** state: `Intruder.{armed,weapon}`, a case-level `threats: Vec<String>`, the `TimelineKind::WeaponDetected` milestone, `TimelineKind::IntruderEnteredZone` (movement into a fresh room/area), and `Intruder.id_quality` / `IntruderDelta.id_quality` (per-frame fitness for ID, drives best-still + forensic-pass timing) |
| `src/engine.rs` | The assessment loop: per-tick multi-camera capture + telemetry → Sonnet live loop → merge into `CaseState` → best stills + Opus forensic; state machine; daily token cap; broadcast + journal. **The live loop fans out ONE focused Sonnet call per active camera, concurrently** (`live_instruction_camera` scopes the model to a single frame), then merges the per-camera `LiveAssessment`s into one via `merge_camera_assessments` (threat=MAX, person=OR, locations concat, threats dedup, intruders reconciled by `id`) before the unchanged `merge_live`/threat machinery runs — better per-frame scrutiny (esp. weapons) than one diluted multi-image call; usage is summed across the calls. **Phase 5**: defines `ControlCommand::{Standdown, Acknowledge}`; `run()` `select!`s a `mpsc::Receiver<ControlCommand>` alongside the HA event stream + tick — `Standdown` reuses the `standdown()` path, `Acknowledge` pushes a `TimelineKind::Acknowledged` milestone + broadcasts (both no-op + log when no case is active). **Intruder movement**: `run_tick` builds `zones_seen` from the per-camera sightings (each camera whose own assessment lists an intruder → its zone via `label_to_zone`, built from `CameraConfig.zone`), then `announce_zone_entries` emits `IntruderEnteredZone` for any fresh zone (zone-level dedup via `ActiveCase.announced_zones`); the trigger zone is seeded as already-announced at case open so the initial location is excluded. While `actively_tracking`, every tick is a full sweep so a newly-entered room isn't motion-gated out. **Alarm mode (0.23.x)**: `handle(AlarmModeChanged)` broadcasts the mode on `mode_tx: watch<AlarmMode>` (HUD/takeover) + opens the case on `Triggered`; on disarm with no case it broadcasts a transient `Authorised`. **15s AUTHORISED dwell**: `revert_pending: (Instant, Revert)` set in `standdown` (`ClearCase` → broadcast `None`) or on a no-incident disarm (`ToDisarmed`); a run-loop timer arm services it; a new case / arm cancels it. `set_mode_tx` wires the channel (daemon only); a startup query seeds the initial mode |
| `src/version.rs` | `ARGUS_VERSION` from `CARGO_PKG_VERSION` |
| `src/state.rs` | `AlarmMode` {Disarmed, Arming, Armed, Triggered, **Authorised** (transient UI-only green after a no-incident disarm)} + `from_ha` + `AlarmTracker` (emits the mode on each change-edge, incl. the first observation) |
| `src/ha/mod.rs` | HA client module; `websocket_url()` (http→ws) |
| `src/ha/ws.rs` | HA WS client: auth handshake, `subscribe_events`/`state_changed`, reconnect+backoff, emits `HaEvent::AlarmModeChanged { mode }` (the whole alarm state machine, not just triggered) over an mpsc channel |
| `src/ha/rest.rs` | `RestClient::snapshot(entity)` → JPEG; `state(entity)` + `telemetry(entities)` → per-tick sensor text |
| `src/llm/mod.rs` | `AnthropicClient::assess(&AssessRequest)` → `Completion { text, usage }`. Raw HTTP to `/v1/messages`; multi-image base64 vision, `cache_control` seed, `Reasoning::{Fast,Deep}` (Sonnet effort-low/no-think vs Opus adaptive+high), optional `output_config.format` structured output |
| `src/out/mod.rs` | Output-sink module (Phase 2a+). Declares `offsite`/`voice`/`voice_policy`/`pagerduty`; sinks consume the `CaseState` stream + case dir, none call the LLM/HA. **Phase 3 `run()`** subscribes to the `watch` channel, diffs the timeline per `case_id` (length cursor), and routes each NEW `TimelineEvent` to the voice gate + PagerDuty as INDEPENDENT channels (one failing never blocks the other). The PagerDuty side runs the full incident lifecycle on the stable `dedup_key=case_id`: `CaseOpened`/material change → `trigger` (update), `Acknowledged` → `acknowledge`, `Standdown`/`Cleared` → `resolve`. Speech is serialised through a single voice worker (mpsc) so milestones don't talk over each other; the worker awaits each `verbalise` (which requests `await_playback=true`, so the RPC returns only after Overwatch finishes PLAYING) — pacing comes from the blocking RPC, NOT a local timing estimate. **`run()` also subscribes to the `watch<AlarmMode>` channel**: on the active→disarmed edge it runs the **disarm** actions Argus took over from HA — stop the klaxon OUT-OF-BAND (direct `SetAlarm(false)`, not via the serial worker, so a stop can't wait behind a playing line), **flush queued intruder lines** (a monotonic `voice_gen` counter; the worker skips any `VoiceMsg::Speak` with an older `generation`), then announce **"Alarm standing down."** Klaxon ON still rides `CaseOpened`; the old klaxon-OFF-on-Standdown-sentinel is gone (out-of-band now) |
| `src/out/voice.rs` | **Phase 3** Overwatch gRPC client. `build.rs` (tonic-build 0.11) compiles `proto/voice.proto` (a relative symlink to `../overwatch/proto/voice.proto`) into the `voice` module (client only). `VoiceClient::{set_alarm,verbalise}` dial `host:port` lazily per call; on connect/RPC failure they log + return (never panic, never block the PD channel). `verbalise` sets `await_playback=true` so the RPC blocks until Overwatch finishes PLAYING the clip — speech is serialised server-side, not by a local timing estimate. It also sets `duck_alarm_volume=duck_volume` (config, default 0.15) so Overwatch ducks any sounding klaxon to that level for the clip and restores it after — the line stays intelligible over the siren without local timing (M1; needs Overwatch ≥0.4.0, older servers ignore the field). `VoiceClient::new_dry()` builds a **dry-voice** client (`--dry-voice`) that LOGS what it would `SetAlarm`/`Verbalise` and never dials |
| `src/out/voice_policy.rs` | **Phase 3** the POSITIVE-ONLY gate: pure `line_for(event, state) -> Option<SpokenLine>`. A whitelist `match` over `TimelineKind` — `case_opened`/`intruder_identified`/`weapon_detected`/`best_still_upgraded`/`security_station_notified` speak; `threat_level_changed` speaks ONLY on escalation (parses the engine's `"<from> → <to>"` detail); `intruder_detected`/`acknowledged`/`standdown`/`cleared` → `None`. `weapon_detected` names the weapon if known. `intruder_entered_zone` → "Intruder in `<zone>`." (terse room-to-room tracking; parses the zone out of the `"Intruder entered <zone>."` detail). Structurally cannot emit a failure line (failures aren't timeline events). Unit-tested (no network) |
| `src/out/pagerduty.rs` | **Phase 3** PagerDuty Events v2 (`reqwest`, reuses the rustls client). `build_event()` (PURE, no network — the shape-test surface) is split from `send()`. Full lifecycle on a **stable dedup_key=`case_id`**: `trigger` on case open + re-trigger (update) on a material change; `acknowledge` on an operator ack (control WS); `resolve` on standdown. Acknowledge/resolve carry NO payload (dedup_key alone moves/closes the incident). `custom_details` (trigger only) = threat level + intruders + recent timeline + the deterministic `s3://<bucket>/<prefix>/<case_id>/` prefix when offsite is enabled (bare prefix — atlas has write-only creds, NO presigned URL). Routing key never logged |
| `src/web/mod.rs` | **Phase 4 + 5** the kiosk HUD HTTP/WS server (`axum` + tower-http `ServeDir`). `serve(cfg, rx, case_base, control_tx)`: `GET /alarm` (+ `/` fallback) → the static `web/` app; `GET /stills/:id` → the JPEG for still `<id>` from the CURRENT case's `stills/<id>.jpg` (resolved via the latest `CaseState` on the watch channel), falling back to scanning every `cases/*/stills/<id>.jpg`; `GET /kiosk` → a WebSocket that consumes the case AND `watch<AlarmMode>` channels and pushes the HUD frame on every change to either: the full serde `CaseState` JSON when a case is present, else a `{"type":"system","mode":...}` frame (arming/armed/authorised/disarmed) the HUD renders as the standby/green pane. **`GET /control` (Phase 5)** → the HA `argus` component's WS: pushes a COMPACT status object (not the full `CaseState`) on connect + every change, and forwards inbound `acknowledge`/`standdown` commands to the engine over `control_tx`. Binds `web.bind` (LAN, shared with `/control` — no separate port). Tolerates client disconnects + a missing `index.html` (404s until the frontend lands). Does NOT call the LLM/HA |
| `src/out/kiosks.rs` | **Phase 4** kiosk-takeover consumer. Consumes BOTH the `watch<Option<CaseState>>` AND the `watch<AlarmMode>` channels and computes a `desired_target`: **HUD** (`<public_base>/alarm`) when a case is present (incl. the cleared/AUTHORISED dwell) OR the alarm is `Arming`/`Armed`/`Triggered`/`Authorised`; else the kiosk's **Dashboard**. Navigates (`shq_display.navigate`) only when the target changes — ONE `/alarm` URL covers arming→triggered→cleared (content switches over the WS, no re-navigation). **LIVE-FIRED on kiosk11** (2026-06-19). nyx caveat applies only to `idle_mode: clock` kiosks (kiosk11 is `off`) |
| `src/out/offsite.rs` | **Phase 2a** offsite S3 replicator: `run(cfg, base, wake)` walks `<base>/cases/`, PUTs each non-`.tmp`/un-`.uploaded` file (events+stills first), drops a `.uploaded` marker, retries with `retry_backoff_secs`. Woken by the `watch` receiver, with a `scan_interval_secs` re-scan fallback (+ startup crash recovery). EXPLICIT static write-only creds (`aws-sdk-s3`, rustls), NOT the ambient chain; relies on bucket-default Object Lock |

## Phase 4 — kiosk HUD (Rust side)

The HUD HTTP/WS server (`src/web/mod.rs`) + the takeover consumer
(`src/out/kiosks.rs`) are spawned in DAEMON mode only: the server iff `web:` is
configured; the takeover consumer iff `web:` is configured AND `kiosks:` is
non-empty. Their `watch::Receiver`s are taken in `main` via `state_tx.subscribe()`
BEFORE `state_tx` moves into the engine (alongside the 2a/3 receivers). The
takeover consumer builds its own `RestClient` (the engine owns the other one).

**Endpoints** (bind `web.bind`, default `0.0.0.0:8770`):

| Endpoint | Serves |
|----------|--------|
| `GET /alarm` (+ `/`) | the static HUD app from `web.dir` (the frontend's `web/`; `index.html` is the SPA) |
| `GET /stills/:id` | `<case_base>/cases/<case_id>/stills/<id>.jpg` as `image/jpeg`; current case first, else a scan of all case dirs; 404 if missing. Accepts `<id>` or `<id>.jpg` |
| `GET /kiosk` | WebSocket — sends the current `CaseState` (or `null`) on connect, then pushes the full serde `CaseState` JSON on every change. Reconnect is the client's job |
| `GET /control` | **Phase 5** WebSocket for the HA `argus` component (same `web.bind` port, NOT a separate listener). See below |

The `/kiosk` WS frame is the SAME serde serialisation as the on-disk journal —
one schema, three surfaces. The frontend references stills by
`intruders[].best_stills[].id` → `/stills/<id>.jpg`.

## Phase 5 — control WS (Rust side)

`GET /control` (on the same `web.bind` HUD server — reuses the Phase-4 axum
router + the `watch<Option<CaseState>>` state; **no second listener/port**). The
HA `argus` component connects here for status + control.

**Server → client** — a COMPACT status object (NOT the full `CaseState`), sent
once on connect and on every `CaseState` change:

```json
{"type":"status","version":"<argus semver>","active":<bool>,"case_id":<string|null>,
 "case_status":<string|null>,"threat_level":<string|null>,"intruder_count":<int>,
 "summary":<string|null>,"updated_at":<string|null>}
```

`active` = a case exists whose status != `cleared`; the projection fields are
null/0 when there is no case. `case_status` (not `status`) carries the case's
status string so it never collides with the message `type`.

**Client → server** — a command: `{"type":"command","command":"acknowledge"}` or
`{"type":"command","command":"standdown"}`. Recognised commands are forwarded to
the engine over the `mpsc::Sender<ControlCommand>` and answered best-effort with
`{"type":"ack","command":<cmd>,"ok":true}`; malformed/unknown frames are logged +
ignored (never panic). `standdown` → the engine's `standdown()` path (same as an
HA `AlarmCleared` edge); `acknowledge` → a `TimelineKind::Acknowledged` milestone
(detail "Operator acknowledged.") + broadcast.

**Wiring:** `main` creates `mpsc::channel::<ControlCommand>()` unconditionally;
the `Receiver` flows into the engine (`run(rx, control_rx)`), the `Sender` is
cloned into `web::serve(...)` only when `web:` is configured (Argus always runs
the web server in production, so the control surface is always present there). No
new config — the control WS shares the `web` port. `--once` does NOT use the
control channel (it calls `run_once`, never `run`).

**nyx dependency (live-takeover blocker, flagged):** a bare
`shq_display.navigate` only does a CDP `Page.navigate`. It does NOT wake a
sleeping kiosk (backlight off) nor kill a Chronos clock overlay (which sits
*above* Chrome on `idle_mode: clock` kiosks), so on those kiosks the takeover is
invisible. nyx's `wake`/`set_display true` is what SIGKILLs Chronos + restores
the backlight (`nyx/CLAUDE.md`, ledger `shq-suite-0001`). Needed before live
takeover: either a small nyx change so `navigate` implies wake+overlay-kill, or a
`shq_display.wake` service the consumer calls first. NOT yet wired (no
`shq_display.wake` exists today) — see `04-kiosk-hud.md` Deviations.

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
./target/release/argus --dry-voice             # daemon, but voice lines + klaxon are LOGGED not spoken
RUST_LOG=argus=debug ./target/release/argus    # verbose (per-state_changed logging)
```

`--dry-voice` is the safe way to see exactly what WOULD be verbalised to Overwatch
during a (scratch-alarm) test: it forces the Phase-3 outputs consumer on and swaps
the real `VoiceClient` for a dry one (`[DRY VOICE] would speak: …` / `would
SetAlarm`). Overwatch is never contacted; PagerDuty still obeys its own
`routing_key` guard (empty = disabled), so a key-less box pages nobody.

`--once` opens an ephemeral case, runs one multi-camera tick, prints the
`CaseState`, and stands down — tests the whole HA + tiered-Anthropic pipeline
without arming the house. Cases are written to `~/.local/share/argus/cases/`.

## Deployment — containerised on atlas (rootless Podman + Quadlet)

**Production runs Argus as a rootless Podman container managed by a systemd
`--user` Quadlet unit on atlas** — the same pattern as atlas's `qdrant`/`rag-serve`
services. (The earlier native-binary path — `cargo build` + `~/.local/bin/argus` +
`argus.service.example` — is superseded; the `.service.example` is kept only as the
non-container reference.)

| Artifact | Role |
|----------|------|
| `Containerfile` | Multi-stage build (rust:slim + `protobuf-compiler` → `debian:bookworm-slim` + `ca-certificates`). **Build context = the repo root** so the `proto/voice.proto` → `../../overwatch/proto/voice.proto` symlink resolves (both trees are copied). Bakes in the `argus` binary + `web/` assets only (~109 MB); config/seed/data are bind-mounted. |
| `argus.container` | Quadlet unit → `~/.config/containers/systemd/argus.container`. `Image=localhost/argus:latest`, `Network=host` (reach HA on `localhost:8123`, the Overwatch RPi over the LAN, bind `:8770`), `EnvironmentFile=%h/.config/argus/argus.env`, `HOME=/root` + read-only mount of `~/.config/argus` and rw mount of `~/.local/share/argus` so the containerised run is byte-identical to native (the config's `~/...` paths land on the mounts). **The HUD `web/` is also bind-mounted** (`~/.config/argus/web` → `/usr/local/bin/web`, ro) so the frontend can be updated by an rsync + reload WITHOUT rebuilding the image (the image's baked `web/` is the fallback). `Restart=always`; `WantedBy=default.target` + linger → boot-persistent. |
| `argus.env.example` | Template for `~/.config/argus/argus.env` (gitignored) — `HA_URL`/`HA_TOKEN`/`ANTHROPIC_API_KEY` (+ optional PD/AWS). systemd injects it as the container env; Argus expands the `${VAR}`s in `config.yaml`. |
| `deploy-container.sh` | One-shot deploy: rsync a minimal context (argus/ minus `target/` + overwatch/proto) to atlas, `podman build` there, install the Quadlet unit, **rsync `web/` to the bind-mount** (`~/.config/argus/web`), `daemon-reload` + `restart`. Refuses to start if `config.yaml`/`argus.env` are absent. Does **not** touch secrets. **Frontend-only tweak?** Just `rsync web/ → atlas:.config/argus/web/` + reload the kiosk — no rebuild. |

```bash
argus/deploy-container.sh                        # build + (re)deploy to jordonsc@atlas.shq.sh
ssh atlas 'systemctl --user restart argus.service'
ssh atlas 'journalctl --user -u argus.service -f' # live logs
```

A redeploy is just re-running the script (rebuilds the image, restarts the unit).
The private `config.yaml`, `seed.md`, and `argus.env` live on atlas under
`~/.config/argus/` (gitignored; mirror to `shq-suite-config`).

## Gotchas

- **HA WS auth order**: server sends `auth_required` → client sends `{type:"auth",access_token}` → expect `auth_ok`. We `subscribe_events` to `state_changed` and filter by `entity_id`, then fire only on the **transition** into `triggered` (the `AlarmTracker` dedupes triggered→triggered attribute-only changes).
- **Reconnect**: backoff resets to 1 s after a successfully-authenticated session, so a transient drop reconnects fast while a down HA backs off (max 30 s).
- **camera_proxy** returns the JPEG in the response body (no temp file) — prefer it over `camera/snapshot`.
- **No Rust Anthropic SDK** → raw `reqwest` HTTP. This is deliberate (see ledger `shq-suite-0002`).
- **Prompt-cache floors differ by model**: Sonnet 4.6 caches a ≥2048-token prefix; **Opus 4.8 needs ≥4096 tokens**. `seed.example.md` (~2265 tokens) caches on Sonnet but is below the Opus floor — the **real premises seed must exceed ~4096 tokens** for the Opus forensic path to get cache reads.
- **Structured output**: the LLM returns JSON via `output_config.format` (`{type:"json_schema", schema}`). Schema rules: every object `additionalProperties:false`, all props `required`, optional fields nullable-but-required (`["string","null"]`), **no** numeric/string constraints (`minimum`/`maxLength`). The model emits *deltas* (`LiveAssessment`/`Identification`); Argus derives the `timeline`.
- **Case dir = upload queue**: each `events/NNNNNN.json` + `stills/*.jpg` is written atomically (`.tmp` then rename) and is an independent unit. Phase 2a adds upload markers + an S3 mirror; don't change the per-file granularity.
- **Motion-gated ticks**: tick 0 (and every `loop_config.full_sweep_every`-th tick) sweeps ALL cameras for a baseline; other ticks assess only cameras with recent motion (`CameraConfig.motion_entities` active within `loop_config.motion_window_secs`) plus any camera tracking an intruder. A camera with no `motion_entities` is always assessed. When nothing is moving, the tick **skips the LLM call entirely** (just cheap state polls) — so an idle triggered house costs ~one baseline call. Partial ticks UPSERT `locations` by label (cameras not in the delta keep their last observation). The Protect `_motion` sensors must be enabled (`switch.<cam>_motion` on) or they read `unavailable` (treated as no-signal).
- **Offsite is write-only by design (Phase 2a)**: `src/out/offsite.rs` builds the S3 client from EXPLICIT static creds (an `s3:PutObject`-only IAM key), never the ambient/instance chain — a compromised atlas must not be able to read or delete prior evidence. The case dir *is* the durable queue: a file with a sibling `<file>.uploaded` marker is done, a `.tmp` is an in-progress write (skip both); failed PUTs retry next pass with `retry_backoff_secs` backoff. Object immutability is the **bucket's** Object Lock — Argus sets no per-object retention. S3 key = `<prefix>/<case_id>/<rest>` (the path relative to `cases/`). The `upload.routine_frames`/`best_stills` throttles are parsed but near-no-ops today (Phase 2 only persists best stills to disk; `#[allow(dead_code)]` on those fields).
- **`watch` channel**: the engine broadcasts `Option<CaseState>` on every mutation; Phases 3/4 subscribe (`state_tx.subscribe()`), not poll. `main` takes `offsite_rx` (2a) and `outputs_rx` (3) BEFORE `state_tx` moves into `Engine::new`.
- **Phase 3 config guards**: voice spawns iff `overwatch:` is present; PagerDuty spawns iff `pagerduty.routing_key` is non-empty. Either absent disables only that channel — never a hard error. Spawned in DAEMON mode only (not `--once`).
- **Proto symlink + tonic version pin**: `argus/proto/voice.proto` is a *relative* symlink to `../overwatch/proto/voice.proto` (same trick as the HA component). `build.rs` runs `tonic_build::configure().build_server(false).compile(...)`. tonic/prost/tonic-build are pinned to **0.11/0.12/0.11** to match Overwatch's Cargo.toml so the stubs are wire-compatible. Needs `protoc` on PATH at build time (system protoc, as Overwatch's `setup-wsl2.sh` installs).
- **Blocking voice pacing (`await_playback`)**: Argus's `verbalise` sets `await_playback=true` (an `optional bool` → `Some(true)` on the generated `VerbaliseRequest`). Overwatch then uses `sink.sleep_until_end()` so the RPC returns only after the clip finishes PLAYING (not just synthesis). The serial voice worker (`src/out/mod.rs`) therefore paces itself by awaiting each RPC — the old chars/sec sleep estimate was REMOVED. Requires Overwatch ≥0.3.0 (the field is back-compatible: an older server ignores the unknown field and reverts to fire-and-forget, restoring the overlap the estimate used to mask).
- **Klaxon ducking during speech (M1, `duck_alarm_volume`)**: `verbalise` also sets `duck_alarm_volume` (config `overwatch.duck_volume`, default 0.15). When >0 Overwatch ducks EVERY active alarm sink to that volume just before the clip and restores it just after — so an intimidation line is intelligible over a sounding klaxon, serialised server-side (no local timing). It's atomic with the blocking play (FIFO on Overwatch's single audio thread: Duck → blocking play → Restore), so the duck only lasts the clip. No-op when no alarm is active. Requires Overwatch ≥0.4.0 (older servers ignore the unknown field — no duck, no error). Only meaningful alongside `await_playback=true` (Argus always sets both); with fire-and-forget the restore would un-duck before the clip ends.
- **`security_station_notified` (M1 direct-speak)**: Phase 3 CANNOT mutate the engine-owned `CaseState`, so it can't push a `SecurityStationNotified` timeline event for the gate to pick up. For M1, the outputs consumer speaks the security-station line **directly** on `CaseOpened` (when it also fires the PD trigger) rather than via the gate. **Noted future:** a feedback channel back to the engine so the event lands in the timeline → HUD ticker (Phase 4) and the gate, instead of a hard-coded direct line.
- **Daemon vs `--once`**: the daemon assesses **every** configured camera on each trigger; the shipped example configures one (so "exactly one assessment per trigger" holds). Multi-camera fan-out is formalised in Phase 2.
- **Per-camera Sonnet fan-out (0.17.0)**: the live loop runs ONE Sonnet call per active still — concurrently (`join_all`), each with a per-camera instruction (`live_instruction_camera`) telling the model it's assessing a SINGLE named frame. A single multi-image call splits the model's attention and dilutes per-frame scrutiny (a held knife gets missed); one focused call per frame fixes it (cost is explicitly NOT a concern). Per-call failures are tolerated (warn + skip that camera). The results are reconciled into one `LiveAssessment` by `merge_camera_assessments` BEFORE the existing `merge_live`/threat machinery runs (unchanged): threat=MAX across calls, person_detected=OR, locations concatenated, threats concat+dedup, intruders grouped by `id` (armed=OR, weapon=first non-empty preferring an armed sighting, confidence=MAX, descriptors/location/activity/best_camera from the highest-confidence sighting). `person_seen`/`model_threat` derive from the MERGED object. Usage is summed across the per-camera calls (one `record_usage` + one `info!("tick:…")`).
- **Weapons & threats (M1.5)**: both the live (Sonnet) and forensic (Opus) prompts explicitly hunt for weapons. The model sets `armed`/`weapon` per person and lists free-text `threats`; `armed` **latches ON** (a later occluded frame never clears it) and the forensic pass can be what first confirms a weapon a grainy live frame missed. Argus enforces a **threat floor** — any armed intruder forces `threat_level` to `Critical` regardless of the model's own call (`enforce_threat_floor`, fires once per case) — and emits a `WeaponDetected` milestone on the unarmed→armed edge, which re-pages PagerDuty and speaks a firm line. `CaseState.threats` re-baselines on a full sweep, unions on a partial tick.
- **Intruder movement / zones (0.20.0, latency-tuned 0.21.0)**: a terse "Intruder in `<zone>`." is spoken the first time an intruder is seen in a fresh zone. A *zone* is a room/area: `CameraConfig.zone` (defaults to the camera `label`), so multiple cameras can share one — the two outdoor-living cameras both map to `Backyard`, the only real overlap. Dedup is **zone-level** (`ActiveCase.announced_zones`), not per-intruder: several people entering one zone speak it once. The **initial trigger zone is excluded** — it is seeded into `announced_zones` at case open from `trigger_location` (and the breach line already names it); if no trigger location is known, the first zone an intruder is seen in is treated as the initial one and suppressed. Zone names must align with the HA area names `input_text.alarm_trigger_room` reports (compared case-insensitively via `zone_key`) or the trigger zone won't be excluded. Voice-only — it does NOT re-page PagerDuty (movement would be noisy). **Latency (0.21.0)**: the announce fires off the **per-camera sighting** for the tick (`zones_seen` built from each camera whose own assessment lists an intruder) — taken in `run_tick` BEFORE `merge_camera_assessments` collapses them to one best-view location — so a room change announces the instant a frame shows the intruder there, not once that camera becomes the dominant view (which lagged ~20 s). It still can't beat the floor of motion-sensor propagation + one cadence + the LLM round-trip (~6–11 s).
- **All-cameras while actively tracking (0.21.0)**: once an intruder is on the roster and was seen within the active window (`DECAY_ELEVATED_SECS`), `run_tick` forces a **full sweep every tick** (`actively_tracking` → `full`), ignoring motion-gating — so the room an intruder just walked into isn't held back waiting on its own smart-detect sensor or the next periodic full sweep. Reverts to motion-gating once the case decays off Critical. Cost (all cameras every ~6 s during a live intrusion) is intentional; helps zone, weapon, and ID latency alike.
- **Alarm-mode takeover (0.23.0)**: Argus watches the WHOLE alarm machine. The engine broadcasts `AlarmMode` on `mode_tx`; the takeover consumer + `/kiosk` WS combine it with the case to decide the kiosk (HUD vs dashboard) and the HUD frame (`CaseState` vs `{type:"system",mode}`). **arming/armed flip the kiosk to the standby pane; triggered to the alarm pane; all on ONE `/alarm` URL** so the content switches over the WS with no reload. The HUD must handle the `system` frame in `render()` (steel standby panes; emerald for `authorised`). Don't assume the `/kiosk` WS only carries `CaseState` any more.
- **15s AUTHORISED dwell (0.23.1)**: every disarm holds a green "all clear" for 15 s before reverting. Post-incident = the cleared `CaseState` (rich); no-incident = a transient `AlarmMode::Authorised` → a generic green system pane. The engine's `revert_pending` timer ends it (`ClearCase`→broadcast `None`; `ToDisarmed`→broadcast `Disarmed`). Re-arming/new-case cancels it. The kiosks consumer keeps the HUD up while a case exists OR the mode is in {Arming,Armed,Triggered,**Authorised**}.
- **Disarm voice moved off HA (0.24.0)**: on the active→disarmed edge the outputs consumer stops the klaxon OUT-OF-BAND (so a stop can't wait behind a blocking `Verbalise`), flushes queued intruder lines via the `voice_gen` generation counter (the currently-playing clip can't be stopped — Overwatch has no interrupt — but everything queued behind it is dropped), and speaks "Alarm standing down." The HA "Alarm Disarmed" automation's `overwatch.verbalise "Security disarmed"` was removed; HA's redundant klaxon-off (`script.alarm_stand_down`) is left as a safety net. **This whole path is dormant while Overwatch voice is muted** (the outputs consumer only spawns when `overwatch:` is configured).
- **Main pane is intruder-centric (0.24.0)**: the HUD's primary camera pane uses `pickPrimaryView` (the most relevant intruder: located → armed → confidence → recent still), NOT a scan of all camera observations — so it follows the latest intruder activity, never an unrelated camera note (e.g. the garage's parked car). Falls back to a person-present location only with no intruders.
- **Best-image selection drives the forensic pass (0.22.0)**: the live Sonnet output carries `id_quality` per intruder (0–1: how good THIS frame is for ID — face/build visible, in focus, close, lit — distinct from `confidence`). `merge_intruders` takes `id_quality`=MAX and points `best_camera` at the **highest-`id_quality`** view, so `pick_still_label` profiles on the CLEAREST frame, not just the first/located camera. `upgrade_intruders` (re)captures a still + runs Opus on first detection, on a weapon edge, OR when `id_quality` beats the last profiled frame by `QUALITY_IMPROVE` (0.12) — chasing a clearer picture rather than a confidence wobble (`ActiveCase.still_quality` tracks the last profiled quality). A clearer frame also makes Opus itself faster + more confident.
- **Identification is announced ONCE, and only when solid (0.21.0 / 0.22.0)**: the Opus pass re-runs as clearer stills arrive (firming up the record), but `IntruderIdentified` — which drives the spoken profile + the PagerDuty page — fires **once per intruder**, and only once `confidence >= ID_SPEAK_CONF` (0.5; validated on live runs — a real intruder sits ~0.9+, a distant false figure ~0.3, so 0.5 cleanly separates them), gated by `ActiveCase.id_spoken` (a `HashSet::insert` after the bar, so `&&` short-circuits to allow a later confident pass to be the first to speak). So the announced profile is a solid one, never a first-glimpse guess; sub-threshold passes update `descriptors`/`dossier`/`spoken_summary` + best-still silently. Mirrors the armed-edge latch on `WeaponDetected`. (`identified` still flips on the first pass for the HUD + the descriptor-clobber guard.)
- **Container build context (the proto-symlink trap)**: the image is built with **context = repo root**, not `argus/`. `argus/proto/voice.proto` is a relative symlink into `../../overwatch/proto/`; a context rooted at `argus/` can't see the target and the tonic build fails. `deploy-container.sh` rsyncs `argus/` *and* `overwatch/proto/` into one tree on atlas and builds there (`podman build -f argus/Containerfile <tree>`). Same `protoc`-at-build-time requirement as the native build.
- **Containerised = native, via HOME mount**: the Quadlet unit sets `HOME=/root` and bind-mounts `~/.config/argus` (ro) + `~/.local/share/argus` (rw) at the container's `/root/...`, so the config's `~/.config/argus/seed.md` and the `ProjectDirs` case dir resolve onto the host exactly as a native run would — **no `--config` and no container-specific config**. `Network=host` is required (rootless, reaches HA loopback + LAN Overwatch + binds `:8770`). Secrets come from `argus.env` via systemd `EnvironmentFile`, never baked into the image.
- **Resident recognition is near-certain only (M1.5)**: the live prompt + seed require a resident match **beyond reasonable doubt** to EXCLUDE a person from `intruders` — a look-alike is still an intruder; **fail toward intruder**. (Real runs are alarm-initiated → assume intrusion.) The richer `authorised: bool`-per-person model + resident reference photos are noted futures (would touch the schema / the Opus call).

## Versioning

Bump `Cargo.toml` `version` on every deployed change (logged at startup as
`Starting Argus vX.Y.Z`). MAJOR/MINOR/PATCH per the root `CLAUDE.md` policy.
