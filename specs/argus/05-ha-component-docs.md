# Phase 5 — HA Component, Deploy & Documentation

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phases 1–4** (a working daemon with
> a `CaseState` channel and a status/control surface to expose).
>
> **Status: ✅ IMPLEMENTED (2026-06-18), Rust build warning-free + Python
> compiles.** HA `argus` component (status sensors + ack/standdown buttons over a
> `/control` WS), the Argus control WS (same `web` port) + control→engine channel,
> the `deploy` native `argus` target, and the cross-project docs. **The real
> premises seed is still OWED** (private, needs the user) — see Follow-ons.
>
> **Goal:** make Argus a first-class citizen — an HA integration for status + control, a deploy-tool
> target, complete documentation, and the **private premises seed** authored for real.

## Context

Argus has run as a standalone daemon through Phases 1–4, driven directly by the HA WS API. Phase 5
keeps HA the coordinator by giving Argus an HA **integration** for observability and control,
wires it into the `deploy` tool, finishes the docs, and authors the real (private) seed so the
assessments are grounded in the actual premises.

## Scope

**In scope:**
- `home-assistant/custom_components/argus/` — WS coordinator (status + arm/ack), mirroring
  `shq_display`/`dosa`.
- An Argus **control WS API** for the component (status broadcast + ack/standdown commands).
- `deploy/` tool: an `argus` target (native build → atlas → systemd restart).
- Docs: `argus/CLAUDE.md`, `argus/README.md`; updates to root `CLAUDE.md` + `docs/Domain.md`.
- The **real premises seed** authored into `shq-suite-config` / wiki `estate/`; the public repo keeps
  `argus/seed.example.md` only.

**Out of scope:**
- Config-flow UI (use YAML config like `shq_display`/`dosa`, not a flow).

## Implementation

### 1. Argus control WS (`src/web/control.rs`)
- A second WS endpoint (or reuse `/kiosk` with a control channel) the HA component connects to:
  push status (`current CaseState` summary, threat level, intruder count, case_id, active?) and
  accept commands (`acknowledge`, `standdown`, optionally `arm`/`disarm` proxied to the HA alarm).
- Coordinator/push pattern + availability timeout, mirroring the existing components.

### 2. HA component (`home-assistant/custom_components/argus/`)
- Files mirror `shq_display`: `__init__.py`, `manifest.json` (YAML config, `version`), `client.py`
  (WS), `coordinator.py` (push + reconnect + availability), and entities:
  - `binary_sensor.py` — Argus active / alarm-case-in-progress.
  - `sensor.py` — threat level, intruder count, current summary, case_id.
  - `button.py` — acknowledge / standdown.
- `configuration.yaml`:
  ```yaml
  argus:
    host: 192.168.1.5   # atlas
    port: <control-ws-port>
    name: "Argus"
  ```
- Document it in `home-assistant/CLAUDE.md` (entities/services/transport) alongside the others.

### 3. Deploy tool (`deploy/`)
- Add an `argus` target: **native** `cargo build --release` (no cross), rsync the binary +
  `web/` assets to atlas, install/restart the systemd unit. Reflect the atlas target in
  `deploy/config/deployment/` (gitignored; mirror to `shq-suite-config`). Note the **non-cross**
  build path explicitly — every other Rust app here cross-compiles to ARM64; Argus does not.

### 4. Documentation
- `argus/CLAUDE.md` — component doc in the house style (source layout, the `CaseState` contract, the
  HA/Anthropic/Overwatch/PagerDuty/kiosk integration points, gotchas, the native-build note).
- `argus/README.md`, `argus/config.yaml.example`, `argus/seed.example.md` (finalised).
- Update root `CLAUDE.md`: add `argus/` to the directory table + the architecture diagram; note it's
  the **only** native (non-cross) Rust app and runs on atlas.
- Update `docs/Domain.md`: add Argus to the apps table and the HA components & transport table.
- If Phase 4 needed a nyx change (wake/kill-Chronos on navigate), document it in `nyx/CLAUDE.md`.

### 5. The real premises seed (private)
- Author the seed into `shq-suite-config` / wiki `estate/` — **never the public repo**: floor plan +
  camera→room map, resident & vehicle **whitelist** (with reference images for Opus to anchor
  identification), and the escalation policy (what counts as critical, what the security station
  should receive). This is a **collaborative pass with the user**; it is the quality ceiling for
  every assessment. Update the wiki `estate/shq.md` security section to mention Argus.

### 6. Ledger + versioning
- Append the rollout to ledger `shq-suite-0002`; flip status to `accepted` once live.
- Bump `argus/Cargo.toml` + `manifest.json` versions per the repo versioning policy.

## Verification (definition of done)
1. HA shows Argus entities (active / threat level / intruder count / summary); ack & standdown
   buttons work.
2. `./deploy argus` builds natively and (re)starts the service on atlas; `sensor` reports the running
   version.
3. Docs updated (root `CLAUDE.md`, `docs/Domain.md`, `argus/CLAUDE.md`, `home-assistant/CLAUDE.md`).
4. The real seed is in place privately and a live trigger produces premises-grounded assessments
   (correct room names, residents/vehicles correctly *excluded* as authorised).

## References
- `home-assistant/custom_components/shq_display/` and `dosa/` (component template).
- `home-assistant/CLAUDE.md`, root `CLAUDE.md`, `docs/Domain.md` (docs to update).
- `deploy/` (the deploy tool), `deploy/config/deployment/*.yaml` (targets).
- wiki `estate/shq.md` (private estate notes — where the seed and the Argus mention live).

---

## Deviations from spec

**Status: ✅ IMPLEMENTED (2026-06-18), Rust build warning-free, Python compiles.**
HA `argus` component + Argus control WS + `deploy` target + cross-project docs.
**The real premises seed remains OWED** (private, needs a collaborative pass with
the user — see Follow-ons).

- **Control WS = same port as the HUD** (decision): rather than a second
  listener, `GET /control` rides the Phase-4 axum server on `web.bind` (default
  `8770`) — the spec's "reuse `/kiosk` with a control channel" option. So the HA
  component connects to `ws://atlas:8770/control`; the control WS only exists when
  `web:` is configured (Argus always runs the web server in production).
- **Control protocol** (`src/web/mod.rs` `/control`): server → client STATUS on
  connect + every `CaseState` change —
  `{"type":"status","version":<argus semver>,"active":<bool>,"case_id":<str|null>,
  "case_status":<str|null>,"threat_level":<str|null>,"intruder_count":<int>,
  "summary":<str|null>,"updated_at":<str|null>}` (`active` = a case exists with
  status ≠ `cleared`; note `case_status` not `status`, to avoid colliding with the
  message `type`). Client → server COMMAND —
  `{"type":"command","command":"standdown"|"acknowledge"}`; best-effort
  `{"type":"ack",...}` reply (consumers don't depend on it).
- **Control → engine channel**: `enum ControlCommand {Standdown, Acknowledge}`;
  `Engine::run` `select!`s a second `mpsc::Receiver<ControlCommand>` (both active
  and idle branches). `Standdown` reuses the existing `standdown()` path (same as
  the HA `AlarmCleared` edge); `Acknowledge` pushes the **additive** new
  `TimelineKind::Acknowledged` ("Operator acknowledged.") + broadcasts. The ack
  kind was added to the Phase-3 voice gate's silent (`None`) arm — an operator ack
  is never spoken over the klaxon, preserving the positive-only gate. `--once` is
  unaffected (it never calls `run`; the control mpsc lives only on the daemon
  path; the web sender is cloned in only when `web:` is set). argus → 0.6.0.
- **HA component** (`home-assistant/custom_components/argus/`, mirrors
  `shq_display`): YAML config `argus: {host, port: 8770, name: "Argus"}`; the same
  `websockets` lib + `DataUpdateCoordinator` reconnect/availability shape; one
  coordinator (Argus is a single daemon, not per-device). Entities:
  `binary_sensor.argus_active` (device_class `safety`; attrs case_id/case_status/
  threat_level/intruder_count/updated_at); `sensor.argus_threat_level`,
  `sensor.argus_intruder_count` (measurement, "intruders"), `sensor.argus_summary`,
  `sensor.argus_case_id`, `sensor.argus_status`; `button.argus_acknowledge`,
  `button.argus_standdown` (fire-and-forget commands). All entities go unavailable
  when the daemon is unreachable. Availability timeout 60 s (vs shq_display's 30 s)
  because Argus status is event-driven, not a periodic broadcast — avoids idle
  flap. manifest `version` `0.1.0`, `iot_class: local_push`, no config flow.
- **Deploy target** (`deploy/`): `./setup argus` (or `argus --build`) does a
  **NATIVE** `cargo build --release` (new `run_cargo_release()` helper — distinct
  from the cross/`build-rpi.sh` path the ARM apps use), rsyncs the binary to
  `~/.local/bin/argus` **and the `argus/web/` HUD assets** to `~/.config/argus/web/`,
  pushes `config.yaml`, installs the systemd **user** unit, restarts. New files:
  `deploy/src/deploy/argus_deployer.py` (`ArgusDeployer`), `config.py`
  (`ArgusConfig`/`get_argus_config`, native defaults: source
  `argus/target/release/argus`, web `argus/web`), the `argus` Click command +
  `run_cargo_release` in `deploy.py`. **Argus is the only non-cross Rust app** —
  expressed per-target (its own deployer + native build fn), no auto-detect.
  **Owed in the gitignored `deploy/config/`** (user-supplied, not committed):
  `deployment/argus.yaml` (atlas host + `auth.username: shq` +
  `~/.ssh/jordon.pem`, mirror `overwatch.yaml`), `app/argus.yaml` (the runtime
  `config.yaml`), `service/argus/argus.service` (mirror
  `argus/argus.service.example`), and on atlas `~/.config/argus/argus.env`
  (`HA_TOKEN`/`ANTHROPIC_API_KEY`, + AWS/PagerDuty keys when those go live).
  Confirm the example unit's `ExecStart` matches `~/.local/bin/argus`.
- **Docs** updated: root `CLAUDE.md` (arch diagram + Argus/atlas + the
  native-not-cross note + hosts), `docs/Domain.md` (apps + HA-components tables),
  `home-assistant/CLAUDE.md` (the `argus` component + entities + control-WS
  transport), `deploy/CLAUDE.md` (the `argus` target + native caveat + gitignored
  config), `argus/CLAUDE.md` (the `/control` endpoint + `ControlCommand`).

## Follow-ons / M2 candidates

**OWED (needs the user, not autonomously buildable):**
- **The real premises seed** — the quality ceiling. A private, collaborative
  authoring pass with the user into `shq-suite-config` / wiki `estate/`: floor
  plan + camera→room map + **per-camera IR/night-vision metadata** + resident &
  vehicle whitelist (with reference images for the Opus pass to anchor on) + the
  escalation policy. **Must exceed ~4096 tokens** so the Opus forensic path gets
  cache reads (Sonnet's floor is 2048; Opus's is 4096 — verified Phase 2). The
  public repo keeps `argus/seed.example.md` only. Update wiki `estate/shq.md`'s
  security section to mention Argus.

**Live-fire verification owed (deferred this autonomous run — residence asleep):**
- A **live alarm trigger** end-to-end (vs `--once`): DoD 2 (a staged intruder
  firming up after the Opus pass; stable ids), the Overwatch klaxon + positive-only
  voice lines, the PagerDuty incident lifecycle, and the kiosk takeover showing the
  HUD — all built + shape-verified, awaiting an authorised waking-hours window.
- **Offsite S3** (Phase 2a): provision the bucket + write-only IAM principal, then
  DoD 1–5 (real-time arrival, kill-atlas partial recovery, write-only can't
  read/delete, outage resume, throttle).
- **PagerDuty routing key** + **Overwatch reachability** for Phase 3 live-fire.

**nyx prerequisite for the kiosk takeover (Phase 4):**
- `shq_display.navigate` must **wake the kiosk + kill the Chronos overlay** or a
  sleeping/clock-mode kiosk won't show the HUD. Preferred: make nyx `navigate`
  imply wake + kill-Chronos + restore `bright_level`; alt: a `shq_display.wake`
  service + wake-then-navigate. Document in `nyx/CLAUDE.md` when it lands.

**Architecture follow-ons:**
- **security_station_notified feedback** (Phase 3): a feedback channel
  engine←outputs so a successful PagerDuty trigger emits the
  `security_station_notified` timeline event (currently the line is spoken
  directly; the event doesn't reach the HUD ticker).
- **A truthful "kiosks taken over" flag** for the HA component (the kiosk consumer
  reports success back) — today it's inferred.
- **HUD**: self-host the fonts (currently a Google Fonts `<link>` — needs kiosk
  internet); a layer-shell HUD overlay for instant stateless dismissal (vs
  navigate-reload); visual sign-off of the HUD with a human via `?demo=1`.

**M1/M2 hardening (from earlier phases):**
- **LTE out-of-band egress** so a WAN cut can't defeat real-time offsite
  replication (Phase 2a is best-effort on a single WAN).
- **Protect smart-detect "best image"** event subscription (vs camera_proxy
  snapshots at cadence) for sharper mugshots.
- Two-way security-station comms / acknowledgement flow (M1 PD is fire-and-forget).
- Multi-tenant / other-premises generalisation; resident-recognition tuning.
