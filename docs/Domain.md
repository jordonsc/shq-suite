# SHQ Suite — Domain Summary

> Read this first. A high-level orientation to what this project is and how the pieces fit. For detail,
> follow the pointers in [Deeper docs](#deeper-docs).

## What it is

**SHQ** ("Superhero HQ") is a home-automation suite built around **Home Assistant as the sole coordinator**.
It is developed for one specific premises without attempting to abstract for general use, but is open-sourced
so others can reuse the work. The suite controls lighting, blinds, displays, doors, climate, audio/alarms,
and assorted devices — all through Home Assistant.

## Mental model / design philosophy

- **Home Assistant is the core and the single source of truth.** Everything routes through it, so keep it
  **clean and simple** — it is the linchpin of the whole system.
- **Avoid third-party hubs and flaky cloud integrations.** Talk to hardware directly and locally for resilience
  and independence: RS485 for blinds, a man-in-the-middle controller for the AC, local endpoints with retry
  hardening where a vendor only offers cloud control.
- **Bespoke ESP32-C6 (PlatformIO) controllers are replacing Matter.** Matter proved too inflexible and the
  commercial vendor-ID certification too costly. This migration is in progress (firmware lives in-repo).
- **"device", unqualified, means the entity in Home Assistant.** It is the first port of call for any
  device-related question or change.
- **Prefer HA Blueprints for new automation work.** They've been adopted with good results — consider whether a
  Blueprint is the right tool before hand-rolling, and advise the user if so.
- **WebSockets over polling** for device comms, where a component needs instant push.

## Moving parts

Applications (top-level directories):

| App | Lang | Role |
|---|---|---|
| `nyx` | Rust | Power/brightness control for wall-display kiosks. |
| `overwatch` | Rust | TTS server + alarm loop — verbalises prompts, raises looping alarms (announcements, security). |
| `dosa` | Rust | **D**oor **O**pening **S**ensor **A**utomation — automated door control via a grblHAL CNC controller. |
| `somfy-sdn` | — | RS485/SDN blind control via bespoke ESP32-C6 WiFi controllers (firmware in-repo). |
| `actron-sniffer` / actron MITM | — | Reverse-engineered man-in-the-middle controller for Actron ducted AC: sits between the Neo controller and the internal unit (MITM because Actron allows only one zone-capable master). |
| `shelly` | Python | CLI to discover, audit, and configure Shelly devices (Gen1 + Gen2) over mDNS. |
| `home-assistant` | Python | The HA custom components that integrate everything above. |
| `deploy` / `setup` | Python | Deployment tool — ships each app to its target device. |

## Home Assistant components & transport

`home-assistant/custom_components/`:

| Component | Transport | Integrates |
|---|---|---|
| `shq_display` | WebSocket | `nyx` — kiosk displays |
| `dosa` | WebSocket | `dosa` — door automation |
| `somfy_sdn` | WebSocket | Somfy RS485 blinds (via the ESP32-C6 controllers) |
| `actron_mitm_controller` | WebSocket | Actron AC MITM controller |
| `overwatch` | sync / executor (not WS) | `overwatch` — TTS + alarms |
| `centurion` | local HTTP + retry | Centurion garage door — cloud integration is unreliable, so it's driven via locally-exposed endpoints, hardened with retry logic |
| `cfa_fire_ban` | — | Fire-ban-day sensor |

## Runbooks

> Generally reliable and meant to be **followed as-is** — agents tend to over-research before executing.
> When a documented runbook covers the task, follow it rather than improvising.

- **Deploy / update a device app** (`nyx`, `overwatch`, `dosa`, …): `./deploy <app> -h <host>` (alias of
  `./setup <app>`). Deploy targets and credentials come from your local `deploy/config` (not committed).
- **Deploy Home Assistant changes** (custom components, etc.): `./deploy ha` — add `--restart` for changes that
  require an HA restart. Note: automations, scripts, and scenes live in the **HA database**, not in YAML, so they
  are changed via the **HA API**, not by editing files.
- **Add a new device**: model it in Home Assistant first; if it needs a bespoke controller, add ESP32-C6
  (PlatformIO) firmware in-repo and a matching HA custom component.

## Deeper docs

- [`docs/Shelly.md`](Shelly.md) — Shelly device management.
- [`docs/actron-local-control.md`](actron-local-control.md) — Actron local control.
- Per-app `README` / `CLAUDE.md` files for each application directory.
