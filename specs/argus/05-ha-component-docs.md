# Phase 5 — HA Component, Deploy & Documentation

> Sub-spec of [Argus master plan](./00-master.md). Depends on **Phases 1–4** (a working daemon with
> a `CaseState` channel and a status/control surface to expose).
>
> **Status: 📝 NOT STARTED.**
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
_(Implementing agent: record the final entity set, the control-WS API, the deploy mechanics, and the
seed structure that worked.)_

## Follow-ons / M2 candidates
_(Implementing agent: capture deferred work — Protect smart-detect best-image subscription,
layer-shell HUD overlay, two-way security-station comms, multi-tenant/other-premises generalisation,
resident-recognition tuning, etc.)_
