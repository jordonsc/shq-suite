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

## Architecture Overview

```
┌──────────────────────────────────────────────────────┐
│                  Home Assistant                       │
│               (atlas.shq.sh:8123)                    │
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
| `nyx/` | Rust | Display server for kiosks — brightness, auto-dim, CDP navigation |
| `overwatch/` | Rust | TTS server + alarm system via AWS Polly, gRPC API |
| `dosa/` | Rust | Door controller via grblHAL CNC, WebSocket API |
| `home-assistant/` | Python | Custom HA integrations for all the above + Centurion garage |
| `deploy/` | Python | SSH/rsync deployment tool for all components |
| `shelly/` | Python | CLI for discovering and configuring Shelly smart devices |

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
- SSH auth via `~/.ssh/jordon.pem`

### Home Assistant Components
- `shq_display`, `overwatch`, `dosa` use YAML config (no config flow)
- `centurion` uses HA config flow (UI-driven setup)
- WebSocket integrations use coordinator pattern with reconnection logic

## Hosts

| Host | Role |
|------|------|
| `atlas.shq.sh` | Home Assistant server |
| `kiosk02-07.shq.sh` | Wall display kiosks (RPi 5 + LCD) |
| `overwatch.shq.sh` | Voice/TTS server (RPi 5, console-only) |
| `kiosk05.shq.sh` | Also runs DOSA door controller |

## Key Gotchas

- **Chrome CDP Host header**: Raw HTTP to Chrome's `/json` must include port in `Host` header (`Host: 127.0.0.1:9222`), else WebSocket URLs get port 80
- **Chrome CDP reads**: Parse `Content-Length` and `read_exact`, never `read_to_end` (hangs waiting for EOF)
- **Cross-compilation**: Uses Podman, not Docker (`CROSS_CONTAINER_ENGINE=podman`)
- **Overwatch proto**: The `.proto` file lives in `overwatch/proto/voice.proto`; the HA component symlinks to it and has generated Python stubs
