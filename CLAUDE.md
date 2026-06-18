# SHQ Suite

Home automation toolsuite ("Superhero HQ") for display kiosks, voice/TTS, automated door control, and smart device management. All controlled via Home Assistant.

## Maintaining CLAUDE.md Files

Each subdirectory has its own `CLAUDE.md` with component-specific documentation. When you make changes to a component, **update its CLAUDE.md to reflect those changes** — particularly:

- New or removed source files
- Changes to API messages, commands, or protocol
- New dependencies in Cargo.toml or manifest.json
- New configuration fields or changed defaults
- New entities, services, or HA integration changes
- Changes to the build or deployment process
- New gotchas or pitfalls discovered during development

Keep the docs concise and factual. Don't pad them out — only document what a future session would actually need to know. If you add a new top-level component, create a CLAUDE.md for it following the same style as the existing ones.

## Versioning

**Bump the semver on every change you flash or deploy** — it's how a running device/HA reports what it is, and how you confirm an OTA/deploy actually took:

- **ESP32 firmware**: the `SOMFY_FW_VERSION` (or equivalent) macro in the component's `src/version.h` (e.g. `somfy-sdn/src/version.h`). Reported in `/stats` `fw=` and the mDNS `fw` TXT. Bumping it also forces a recompile, so the appended build date refreshes (sidesteps the `__DATE__` content-hash cache gotcha).
- **HA custom components**: the `version` field in `custom_components/<name>/manifest.json`.

Use MAJOR.MINOR.PATCH: MAJOR = breaking API/protocol change, MINOR = new back-compatible feature, PATCH = bug fix. Increment fw and HA-component versions independently (they're separate artifacts).

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│                  Home Assistant                       │
│               (redacted.host:8123)                    │
│                                                      │
│  ┌────────────┐ ┌──────────┐ ┌──────┐ ┌──────────┐  │
│  │ shq_display│ │overwatch │ │ dosa │ │centurion │  │
│  │ (WS:8765) │ │(gRPC:    │ │(WS:  │ │(HTTP)    │  │
│  └─────┬──────┘ │50051)    │ │8766) │ └─────┬────┘  │
│        │        └────┬─────┘ └──┬───┘       │       │
└────────┼─────────────┼──────────┼───────────┼───────┘
         │             │          │           │
    ┌────▼────┐   ┌────▼────┐  ┌─▼──┐   ┌────▼────┐
    │   Nyx   │   │Overwatch│  │DOSA│   │Centurion│
    │(kiosks) │   │ (voice) │  │    │   │(garage) │
    └─────────┘   └─────────┘  └────┘   └─────────┘
```

## Directory Structure

| Directory | Language | Description |
|-----------|----------|-------------|
| `nyx/` | Rust | Display server for kiosks — brightness, auto-dim, CDP navigation, clock-screensaver control |
| `chronos/` | Rust | Fullscreen clock overlay (wlr-layer-shell) shown over the kiosk dashboard as an optional idle screensaver; spawned/killed by nyx for kiosks in `idle_mode: clock` |
| `overwatch/` | Rust | TTS server + alarm system via AWS Polly, gRPC API |
| `dosa/` | Rust | Door controller via grblHAL CNC, WebSocket API |
| `home-assistant/` | Python | Custom HA integrations for all the above + Centurion garage |
| `deploy/` | Python | SSH/rsync deployment tool for all components |
| `shelly/` | Python | CLI for discovering and configuring Shelly smart devices |
| `actron-sniffer/` | C++ (Arduino/PlatformIO) | RS485 sniffer + MITM bridge (ESP32-C6 / TinyC6) for the Actron NEO↔indoor protocol. Dual-purpose: HTTP RE toolkit + WebSockets Controller API on port 8767 for the `actron_mitm_controller` HA integration. |
| `somfy-sdn/` | C++ (Arduino/PlatformIO) | ESP32-C6 controller for Somfy SDN RS485 blind motors; `actron-sniffer` design twin (HTTP debug + WS:8767 + HA `cover`). Normal bus participant (not a MITM); ports the SDN protocol from the separate `matter-apps` repo (`common/features/app_sdn.cpp`). Firmware + `somfy_sdn` HA component (zeroconf-discoverable) **hardware-verified** on a TinyC6 + live motor. Design in [`somfy-sdn/SPEC.md`](somfy-sdn/SPEC.md). |

## Reference Docs

| Doc | Description |
|-----|-------------|
| [`docs/actron-local-control.md`](docs/actron-local-control.md) | Actron A/C hardware identity, comms architecture, and the ICUNO-MOD Modbus path for local control (incl. register map + the per-zone-temp limitation) |

## Common Patterns

### Rust Applications (nyx, overwatch, dosa)
- All target **Raspberry Pi 5 ARM64** via `cross` with Podman (not Docker)
- Cross-compile: `cd <app> && ./build-rpi.sh`
- Build output goes to `<app>/build/` for deployment
- All use `tokio` async runtime, `tracing` for logging, `serde` for JSON
- Run with `RUST_LOG=info` (or `RUST_LOG=<app>=debug`)
- No test suites — tested manually on hardware

### Deployment
- Deploy tool is symlinked as `./setup` in project root
- Sensitive config lives in `deploy/config/` (gitignored)
- All services run as **systemd user services** under the `shq` user
- SSH auth via `~/.ssh/jordon.pem` (username `shq` for kiosks/overwatch/dosa, `jordonsc` for HA). The deploy targets/keys live in `deploy/config/deployment/*.yaml` (gitignored; mirrored in `shq-suite-config`)

### Home Assistant Components
- `shq_display`, `overwatch`, `dosa` use YAML config (no config flow)
- `centurion` uses HA config flow (UI-driven setup)
- WebSocket integrations use coordinator pattern with reconnection logic

## Hosts

| Host | Role |
|------|------|
| `redacted.host` | Home Assistant server |
| `redacted.host` | Wall display kiosks (RPi 5 + LCD) |
| `redacted.host` | Voice/TTS server (RPi 5, console-only) |
| `redacted.host` | Also runs DOSA door controller |

## Home Assistant API Access

Claude Code has direct access to the HA REST API via the `./ha` helper script (uses `$HA_URL` and `$HA_TOKEN` env vars from `~/.bashrc`). An MCP server is also configured in `~/.claude.json` for basic device control. See `home-assistant/CLAUDE.md` for API usage details.

## Key Gotchas

- **Chrome CDP Host header**: Raw HTTP to Chrome's `/json` must include port in `Host` header (`Host: 127.0.0.1:9222`), else WebSocket URLs get port 80
- **Chrome CDP reads**: Parse `Content-Length` and `read_exact`, never `read_to_end` (hangs waiting for EOF)
- **Cross-compilation**: Uses Podman, not Docker (`CROSS_CONTAINER_ENGINE=podman`)
- **Overwatch proto**: The `.proto` file lives in `overwatch/proto/voice.proto`; the HA component symlinks to it and has generated Python stubs
