# Argus — AI-Powered Alarm System (Master Plan)

> **Read this first.** High-level design and the phased-delivery index for **Argus**, an AI alarm
> layer over the existing Home Assistant alarm. Each phase has its own self-contained sub-spec in
> this folder, written so a **fresh agent can implement it in a clean session**. Ledger:
> [`shq-suite-0002`](ledger) records the design decision and the four locked choices.

## What it is

When the house alarm fires, Argus takes over: it pulls **camera stills + sensor telemetry**, feeds
them to a **frontier multimodal LLM** for real-time *what / where / who* assessment, takes over the
**wall kiosks** with a sci-fi HUD that shows the running assessment and the best identifying stills,
**verbalises positive progress** through Overwatch to intimidate intruders, and **dispatches a
dossier to a security station** (PagerDuty for M1).

Argus **consumes** the existing alarm — it does not replace it. The HA `alarm_control_panel.shq_alarm`
state machine and the 11 UniFi Protect cameras already exist; Argus is the intelligence and theatre
layer on top.

## Locked design decisions (user-selected, 2026-06-18)

| # | Decision | Choice |
|---|----------|--------|
| 1 | **Kiosk HUD rendering** | **Vector chrome** (SVG/CSS/Canvas) themed by a CSS custom property. `alarm.png` (red) and `authorised.png` (green) are **the same parametric frame** in two hues — built as one component, `--hud: crimson \| emerald`. The `docs/concept/*.png` are **spec only**, never baked backgrounds. Camera stills/mugshots are the only raster content, dropped into the drawn frame. Ships as a web app the kiosks display. |
| 2 | **Model strategy** | **Tiered, both Anthropic.** `claude-sonnet-4-6` ($3/$15) for the high-frequency live *what/where* loop; `claude-opus-4-8` ($5/$25) for forensic *who* identification on best stills (high-res 2576px vision + strongest reasoning) and the security-station dossier. Premises **seed** prompt is `cache_control`-cached per model (caches are model-scoped, 5-min TTL kept warm by the running loop → ~90%-off reads). **Fable 5 rejected** (2× Opus price + safety classifiers risk false-positive refusals on security-adjacent prompts). Rust has no official Anthropic SDK → **raw HTTP** (`reqwest`) against `/v1/messages`. |
| 3 | **Controller host** | **`atlas`** (`192.168.1.5`, the HA + RAG server, UniFi Buffer-1 power). **Implication: `atlas` is x86_64 Ubuntu, NOT RPi ARM64** — Argus is a **native `cargo build --release` + systemd service**, *not* a `cross`/Podman RPi cross-compile like nyx/overwatch/dosa. HA API calls are localhost (low-latency stills). Tradeoff: adds load to the SPOF. |
| 4 | **M1 scope** | **All four capabilities**: (a) kiosk takeover + HUD, (b) LLM assessment loop, (c) Overwatch positive-only intimidation voice, (d) PagerDuty security station. **+ Trigger profiles** (promoted into M1 2026-06-19): a tiered response keyed on *why* Argus was woken — `Alarm` / `Investigate` / `General` (Phase 7). |

## Architecture

```
alarm_control_panel.shq_alarm ──triggered──► Home Assistant (atlas)
            │  (Argus subscribes directly to the HA WS API)
            ▼
        ┌─────────────────── ARGUS (Rust daemon, atlas) ──────────────────┐
        │  • HA WS client : alarm state + sensor telemetry                 │
        │  • HA REST       : camera stills (/api/camera_proxy/<entity>)    │
        │  • LLM loop      : Sonnet 4.6 (live what/where)                  │
        │                    Opus 4.8 (forensic who + dossier)            │
        │                    premises SEED prompt-cached per model         │
        │  • CaseState machine + event log (the downstream contract)       │
        │  • HTTP server   : kiosk web app + still images                  │
        │  • WS server     : push CaseState to kiosks + the HA component   │
        └─┬───────────┬──────────────┬─────────────┬──────────────┬────────┘
          │ gRPC      │ shq_display.  │ PagerDuty   │ argus HA     │ aws-sdk-s3
          ▼           ▼ navigate (HA) ▼ Events v2   ▼ component(WS) ▼ (real-time,
        overwatch   nyx → kiosks    security      status +        write-only)
       (Verbalise/ (flip to alarm   station       arm/ack      ┌──────────────┐
        SetAlarm)   web app URL)    (dossier)     in HA        │ AWS S3 (off- │
                                                                │ site, immut- │
                                                                │ able case)   │
                                                                └──────────────┘
   atlas is on-prem & inside the threat model → the case is mirrored offsite as it is produced.
```

## Components (built across the phases)

| Component | Path | Lang | Phase |
|-----------|------|------|-------|
| Argus daemon | `argus/` | Rust (native x86_64) | 1–4 |
| Kiosk alarm web app | `argus/web/` (served by Argus) | HTML/CSS/SVG/JS | 4 |
| Offsite case store | AWS S3 (immutable, write-only creds) | — | 2a |
| HA integration | `home-assistant/custom_components/argus/` | Python | 5 |
| Premises **seed** | private — `shq-suite-config` / wiki `estate/` | Markdown | 1 (template) / ongoing |
| Deploy target | `deploy/` extension (`argus` → atlas, native + systemd) | Python | 1/5 |

## Reused existing pieces (do not reinvent)

- **`alarm_control_panel.shq_alarm`** + a Protect alarm manager — the alarm state machine already
  exists; Argus subscribes to it via the HA WS API.
- **11 UniFi Protect cameras** in HA (perimeter ×2, front yard, outdoor living ×2 incl. a PTZ,
  garage, laundry, kitchen, hallway, stair landing). Stills via `/api/camera_proxy/<entity>`.
- **Overwatch** gRPC (`overwatch/proto/voice.proto` — `Verbalise` / `SetAlarm` / `PlayTone`), port 50051.
- **nyx** CDP `Page.navigate` + the `shq_display.navigate` HA service for the kiosk takeover. The
  Chronos reload concern (ledger `shq-suite-0001`) is **screensaver-specific and does not apply** to
  a deliberate alarm takeover.

## Cross-cutting conventions (every phase)

- **Build/deploy:** native `cargo build --release` on (or for) `atlas`; **no `cross`/Podman**. Runs
  as a **systemd service**; persistent config at `~/.config/argus/config.yaml` (mirrors nyx's
  `~/.config/shqd/`). Extend the `deploy/` tool with an `argus` target.
- **Stack:** `tokio`, `tracing`, `serde`, `reqwest` (Anthropic + PagerDuty + HA REST),
  `tokio-tungstenite` (HA WS client + kiosk WS server), `tonic` + `tonic-build` (Overwatch gRPC,
  reuse `voice.proto`), `axum` (static web app + WS endpoints), `serde_yaml` (config).
- **Anthropic:** raw HTTP to `https://api.anthropic.com/v1/messages`; headers `x-api-key`,
  `anthropic-version: 2023-06-01`. Models `claude-sonnet-4-6` / `claude-opus-4-8`. `system` is an
  array with `cache_control: {type: "ephemeral"}` on the seed block. Vision = base64 `image/jpeg`
  blocks. Use `output_config.format` (json_schema) for **structured assessments** so Argus parses
  reliably. Adaptive thinking (`thinking: {type: "adaptive"}`) on the Opus identification calls.
  **No** `temperature`/`top_p`/`budget_tokens` (these 400 on 4.8). Keep `max_tokens` modest
  (assessments are short, ~1–2k).
- **Secrets** (never committed): `ANTHROPIC_API_KEY`, `PAGERDUTY_ROUTING_KEY`, a long-lived
  `HA_TOKEN`. Sourced via the existing secret approach; ship `argus/config.yaml.example` with
  placeholders. Mirror real config into `shq-suite-config`.
- **atlas is inside the threat model → the case must survive it.** atlas is on-prem and an intruder
  can destroy it mid-incident. The case (assessments + stills + dossier) is replicated to an
  **immutable offsite store in real time** (Phase 2a), using **write-only** credentials so a
  compromised atlas cannot tamper with or delete prior evidence. Evidence durability outranks the
  intimidation theatre — Phase 2a builds before Phases 3–4.
- **The premises SEED is private and is the product's quality ceiling.** Floor plan, camera→room
  map, **camera imaging / IR night-vision metadata** (so the model doesn't misread IR illuminator
  reflections off vehicle bodywork/headlight optics/number plates as lights being "on" — a verified
  Phase-1 false positive), resident & vehicle whitelist (with reference images for Opus to anchor
  on), and the escalation policy. Lives in `shq-suite-config` / wiki `estate/`, **never** the public
  repo. The public repo ships `argus/seed.example.md` only. **Note:** the seed must exceed Sonnet's
  ~2048-token cache floor for the prompt-cache economics to apply (the example template is below it).
- **Versioning:** bump `argus/Cargo.toml` semver on every flashed/deployed change; HA component
  version in `custom_components/argus/manifest.json` independently. Maintain `argus/CLAUDE.md` and
  per-component docs as you go.

## Phased delivery

| Phase | Spec | Goal | Status |
|-------|------|------|--------|
| 1 | [`01-foundation.md`](./01-foundation.md) | Skeleton + HA client + Anthropic vision: **trigger → still → assessment** on one camera, end-to-end | ✅ Implemented & verified (2026-06-18) — DoD 1–3 confirmed live (build, real Sonnet assessment, seed cache write→read); only DoD 4 (live trigger) unrun. Cache floor finding: real seed must exceed ~2048 tokens |
| 2 | [`02-assessment-loop.md`](./02-assessment-loop.md) | Full multi-camera + telemetry loop; tiered Sonnet/Opus; prompt-cached seed; the **`CaseState`** contract | ✅ Implemented & verified live (2026-06-18) — structured `CaseState` via `output_config.format`, case-dir journal, `watch` broadcast; DoD 2 (staged intruder) + live trigger deferred. ⚠️ Opus cache floor is 4096 tokens (Sonnet 2048) |
| 2a | [`02a-case-resilience.md`](./02a-case-resilience.md) | **Real-time offsite replication** of the case to S3 (immutable, write-only creds) — so the case survives destruction of atlas. **Build before Phases 3–4.** | ✅ Implemented (2026-06-18), build warning-free — `aws-sdk-s3` replicator off the case dir (write-only static creds, events+stills first, `.uploaded` markers, watch-woken + re-scan). Live S3 deferred (AWS creds + bucket/IAM owed) |
| 3 | [`03-outputs.md`](./03-outputs.md) | Overwatch **positive-only** voice + PagerDuty trigger/resolve with the dossier | ✅ Implemented & shape-verified (2026-06-18), build warning-free, 9 unit tests pass — tonic 0.11 voice client (proto symlinked), pure positive-only gate, PagerDuty Events v2 (`build_event`/`send` split), `out::run` timeline-diff wiring (independent voice + PD channels). **Live-fire deferred** (no real klaxon/page — residence asleep); owed: Overwatch reachable + real `PAGERDUTY_ROUTING_KEY` |
| 4 | [`04-kiosk-hud.md`](./04-kiosk-hud.md) | Vector HUD web app + kiosk takeover via nyx + live `CaseState`/stills push | ✅ Implemented + **LIVE-FIRED on kiosk11** (2026-06-19) — axum `/alarm`+`/stills/:id`+`/kiosk` WS server + the vector HUD (`argus/web/`, redesigned this session) + `shq_display.navigate` takeover, now driven by the WHOLE alarm machine (arming/armed/triggered/authorised standby panes + 15s green dwell). nyx wake/Chronos caveat applies only to `idle_mode: clock` kiosks (kiosk11 is `off`) |
| 5 | [`05-ha-component-docs.md`](./05-ha-component-docs.md) | `argus` HA component (status + arm/ack), deploy tool, docs, private seed | ✅ Implemented (2026-06-18), Rust build warning-free + Python compiles — HA `argus` component (status sensors + ack/standdown buttons over a `/control` WS on the web port), control→engine channel (`ControlCommand`, `TimelineKind::Acknowledged`), native `deploy` `argus` target, cross-project docs. **Real premises seed still OWED** (private, needs the user) |
| 6 | [`06-refinements.md`](./06-refinements.md) | Post-M1 live-hardening + deployment log (NOT a feature phase — the running record of fixes since M1, plus the **containerisation** of Argus on atlas) | 🔧 Active log — argus **0.22.2**, containerised on atlas (ledger `shq-suite-0004`). Read first for current state + the TEST posture |
| 7 | [`07-trigger-profiles.md`](./07-trigger-profiles.md) | **Trigger profiles** — tiered response (`Alarm` / `Investigate` / `General`) keyed on the trigger; output-gating + escalation for the softer signals | 📋 Design complete, **M1 scope** (promoted 2026-06-19) — NOT yet implemented |

Phases 1–5 are **strictly ordered** — each consumes the prior phase's output. The
**`CaseState` schema defined in Phase 2 is the central contract** that Phases 3 and 4
render; Phase 7 extends it (a `trigger_profile` field + output gating). Phase 6 is a
log, not a feature phase.

## Reading guide for a fresh agent picking up Phase N

1. **This master** — the locked decisions and cross-cutting conventions above are load-bearing.
2. **The Phase N spec** — your implementation contract.
3. **The Phase N-1 spec's "Deviations from spec" + "Inputs to Phase N" sections** — the
   authoritative record of how the live code differs from what was drafted. Trust these over the
   master where they conflict.
4. **`search_corpus` / ledger `shq-suite-0002`** before grepping — the corpus indexes the whole repo.
5. The relevant existing component's `CLAUDE.md` (`nyx/`, `overwatch/`, `home-assistant/`).

## Project status

**2026-06-19 — UPDATE.** M1 phases 1–5 are live-validated end-to-end against the
**real** alarm + Overwatch; argus is now **0.24.0**, **containerised on atlas**
(rootless Podman + systemd Quadlet, ledger `shq-suite-0004`). A large post-M1
hardening pass (per-camera fan-out, threat ratchet, blocking voice, intruder-movement
zone announcements, quality-aware forensic ID, **alarm-mode kiosk takeover LIVE-FIRED
on kiosk11** with arming/armed/authorised standby panes + 15s green dwell, **disarm
logic moved off HA into Argus**, HUD redesign) is logged in
[`06-refinements.md`](./06-refinements.md) — **read it first** for current state + the
deliberate TEST posture the house is in (incl. Overwatch voice MUTED for the session).
**M1 scope was extended** with the **trigger profiles**
([`07-trigger-profiles.md`](./07-trigger-profiles.md), design complete, pending build).
Remaining M1 / M2 / M3 work is the structured roadmap in
[§ Milestone task lists](#milestone-task-lists) below (M1: PagerDuty live-fire, trigger-profile
build, kiosk 02–10 migration, klaxon-volume ducking, resident photos, the Argus–Nyx link).
Branch `argus-design-specs`, committed — not yet pushed.

**2026-06-18 — ALL PHASES (1, 2, 2a, 3, 4, 5) IMPLEMENTED & COMMITTED** on branch
**`argus-design-specs`** (not yet pushed). `cargo build --release` warning-free
(argus **0.6.0**); the HA `argus` component + deploy tool compile. Ledger:
`shq-suite-0002`.

**Decided values** (don't re-litigate): models `claude-sonnet-4-6` (loop) +
`claude-opus-4-8` (ID); host **atlas**, native x86_64 build (NOT cross — the only
non-cross Rust app here); offsite store **AWS S3, `ap-southeast-2`, Object Lock
compliance mode, 1-year retention, write-only creds**; kiosk HUD = vector themed
component (crimson/emerald, `argus/web/`); the control WS + HUD share the `web`
port (default `8770`); M1 = all four outputs + Phase 2a resilience.

**Live-verified:** Phase 1 (trigger→still→assessment) + Phase 2 (`--once`:
structured `CaseState`, Sonnet prompt-cache hit, case-dir journal). **Everything
else is built + shape-verified with live-fire DEFERRED** to an authorised
waking-hours window (the residence was asleep): a live alarm trigger, the
Overwatch klaxon/voice, the PagerDuty incident, the kiosk takeover, and offsite
S3 (needs the bucket + write-only IAM). See each phase's Deviations + 05's
Follow-ons.

**Cache floors (verified):** Sonnet 4.6 caches a ≥2048-token seed; **Opus 4.8
needs ≥4096 tokens** — the real seed must exceed ~4096 for the Opus path to cache.

**Owed separately (needs the user):** the **real premises seed** — a private
authoring pass (floor plan, camera→room map, camera imaging/IR night-vision
metadata, resident/vehicle whitelist + reference images, escalation policy) into
`shq-suite-config`/wiki — the quality ceiling for every assessment. The public
repo ships only `argus/seed.example.md`. **nyx prerequisite:** `shq_display.navigate`
must wake + kill-Chronos for the kiosk takeover to show on sleeping/clock kiosks (the
technical dependency behind the M1 **Argus–Nyx link** task).

## Milestone task lists

The canonical Argus roadmap. Phases 1–6 delivered the M1 **core loop** (live-validated,
containerised on atlas); the lists below are the **remaining** work, grouped by milestone.
Status: ✅ done · 🔧 partial / in-progress · 📋 designed, not built · 🆕 new, not started.

### M1 — functioning prototype
The full working flow for a functioning prototype. The four core capabilities are already
live; these complete it.

* 🆕 **Reduce klaxon volume when verbalising** — duck the klaxon while a line plays, built
  into Overwatch. (Today the klaxon only *stops* out-of-band on disarm, 0.24.0; it does not
  duck during speech.)
* 🔧 **PagerDuty flow: create, update and close alerts** — Phase 3 ([`03-outputs.md`](./03-outputs.md))
  implements Events v2 trigger/resolve + re-page on `WeaponDetected`; owed: a real
  `PAGERDUTY_ROUTING_KEY` + live-fire, plus the explicit acknowledge/update path.
* 📋 **Trigger profiles** (`Alarm` / `Investigate` / `General`) — design complete in
  [`07-trigger-profiles.md`](./07-trigger-profiles.md), not yet built.
* 📋 **Resident reference photos** — attach resident photos to the Opus identify (and maybe
  the live) call to anchor recognition; store privately (shq-suite-config, not the public
  repo). Also the main lever left on the weapon/identity ceiling; the richer
  `authorised: bool`-per-person model is the related `CaseState` schema option. Design + build pending.
* 🔧 **Kiosk monopolisation via Argus (remove the current approach)** — Argus drives the
  takeover on kiosk11 today. **Scaffolding + cutover runbook DONE** (config-example entries
  for kiosk02–10 added; the live HA path enumerated read-only — takeover is `script.kiosks_alarm`
  from "Alarm Armed" id `1770878937433`, restore is `script.kiosks_dashboard` from "Alarm
  Disarmed" id `1770879164083`). **Remaining: the live morning cutover** (add 02–10 to the
  atlas Argus config, reflash nyx 1.2.0, redeploy Argus, retire the two HA script calls) —
  full step/verify/rollback runbook in [`06-refinements.md`](./06-refinements.md) §
  "Kiosk 02–10 cutover runbook". Kiosks 02–10 are in active use → do it with the user present.
* 🆕 **Argus–Nyx link** — depends on the nyx `shq_display.navigate` wake + kill-Chronos
  prerequisite:
  - wake the screen when switching alarm mode
  - allow normal Nyx screen blank (clock / off) while armed
  - force the screen on while in alarm mode

**M1 open question:** Argus **replacing vs augmenting** the legacy
`script.alarm_trigger_actions` (the production-model decision — see
[`06-refinements.md`](./06-refinements.md) TEST POSTURE).

### M2 — productisation
Productisation toward an enterprise-level solution. Argus becomes its own product, out of
the shq-suite.

* **S3 live push** — the Phase 2a local journal / upload queue already exists
  ([`02a-case-resilience.md`](./02a-case-resilience.md)); the actual offsite replication
  (bucket + write-only IAM) is **deferred from M1 to here**.
* **Event database** — durable store of incidents / cases.
* **User accounts & a web interface** for browsing incidents (consider a `js_web` tenant for this).
* **Update the "SHQ Display" HA integration:**
  - gets bundled into Argus
  - displays become device-orientated
  - device auto-discovery
* **LTE out-of-band egress** so a WAN cut can't defeat offsite replication.

### M3 — commercial product
Commercial product offering.

* Argus product website (`js_web`).
* Commercial rollout strategy.
* Legal.
* DR (disaster recovery).
* Security review (preferably Fable).
