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
┌──────────────────────────────────────────────────────────────────┐
│                        Home Assistant                              │
│                     (redacted.host:8123)                           │
│                                                                    │
│  ┌────────────┐ ┌──────────┐ ┌──────┐ ┌──────────┐ ┌──────────┐  │
│  │ shq_display│ │overwatch │ │ dosa │ │centurion │ │  argus   │  │
│  │ (WS:8765) │ │(gRPC:    │ │(WS:  │ │(HTTP)    │ │(WS:8770) │  │
│  └─────┬──────┘ │50051)    │ │8766) │ └─────┬────┘ └─────┬────┘  │
│        │        └────┬─────┘ └──┬───┘       │            │       │
└────────┼─────────────┼──────────┼───────────┼────────────┼───────┘
         │             │          │           │            │
    ┌────▼────┐   ┌────▼────┐  ┌─▼──┐   ┌────▼────┐   ┌────▼─────┐
    │   Nyx   │   │Overwatch│  │DOSA│   │Centurion│   │  Argus   │
    │(kiosks) │   │ (voice) │  │    │   │(garage) │   │ (atlas)  │
    └─────────┘   └─────────┘  └────┘   └─────────┘   └──────────┘
```

Argus is unusual: the daemon runs **on the HA host itself (atlas)**, and HA also *consumes* it — Argus watches the alarm over the HA WS/REST API and pushes case status back to HA via the `argus` component's control WebSocket (default `8770`).

## Directory Structure

| Directory | Language | Description |
|-----------|----------|-------------|
| `nyx/` | Rust | Display server for kiosks — brightness, auto-dim, CDP navigation, clock-screensaver control. **→ Authoritative in the Argus repo** (`jordonsc/argus`); build/deploy from there. This copy is in-sync but secondary. |
| `chronos/` | Rust | Fullscreen clock overlay (wlr-layer-shell) shown over the kiosk dashboard as an optional idle screensaver; spawned/killed by nyx for kiosks in `idle_mode: clock`. **→ Authoritative in the Argus repo**; build/deploy from there. |
| `overwatch/` | Rust | TTS server + alarm system via AWS Polly, gRPC API. **⚠️ Stale copy — do not build/deploy from this repo.** Overwatch's live source + deploy moved to the **Argus project** (`jordonsc/argus`, `/mnt/t/Repos/argus/overwatch`), which is ahead of this tree and is Overwatch's primary consumer. Build/deploy via `./setup overwatch` **from the Argus repo**; edit config at `argus/deploy/config/app/overwatch.yaml`. See `overwatch/CLAUDE.md`. |
| `dosa/` | Rust | Door controller via grblHAL CNC, WebSocket API |
| `argus/` | Rust (native x86_64) | **⚠️ Pre-split monolith (v0.36.0) — development moved out:** the thin-agent split put the **edge agent** in the **Argus repo** (`jordonsc/argus`, v3.x) and the **command backend** in **js_web** (`services/argus/`). Build/deploy from the Argus repo, not here. This row describes the pre-split design, kept for history. — AI alarm assessment daemon on **atlas** — on alarm `triggered`, captures camera stills and feeds them to Anthropic Claude with a private premises seed for a real-time *who/what/where* assessment (a tiered Sonnet live loop + Opus forensic ID), kept in a `CaseState`. Consumers: offsite S3 evidence, Overwatch voice + PagerDuty dispatch, a kiosk HUD web app (served from `argus/web/`), and an `argus` HA component. The **only native (non-`cross`) Rust app** — `cargo build --release`, packaged as a **rootless Podman container** (systemd `--user` Quadlet on atlas; NOT an RPi/`cross` build). Phases 1–5 + the Phase-8 post-live-test hardening done and **live-fire validated (v0.36.0, merged to `main` 2026-06-21)**; LLM prompts live in `argus/prompts.yaml`. Phased design in [`specs/argus/`](specs/argus/00-master.md); latest in [`08-post-test-hardening.md`](specs/argus/08-post-test-hardening.md). |
| `home-assistant/` | Python | Custom HA integrations for all the above + Centurion garage |
| `deploy/` | Python | SSH/rsync deployment tool for all components |
| `shelly/` | Python | CLI for discovering and configuring Shelly smart devices |
| `actron-sniffer/` | C++ (Arduino/PlatformIO) | RS485 sniffer + MITM bridge (ESP32-C6 / TinyC6) for the Actron NEO↔indoor protocol. Dual-purpose: HTTP RE toolkit + WebSockets Controller API on port 8767 for the `actron_mitm_controller` HA integration. |
| `somfy-sdn/` | C++ (Arduino/PlatformIO) | ESP32-C6 controller for Somfy SDN RS485 blind motors; `actron-sniffer` design twin (HTTP debug + WS:8767 + HA `cover`). Normal bus participant (not a MITM); ports the SDN protocol from the separate `matter-apps` repo (`common/features/app_sdn.cpp`). Firmware + `somfy_sdn` HA component (zeroconf-discoverable) **hardware-verified** on a TinyC6 + live motor. Design in [`somfy-sdn/SPEC.md`](somfy-sdn/SPEC.md). |
| `skopos/` | Rust | **Spec only — no code yet.** mmWave sensing nodes: Raspberry Pi 5 + DreamHAT+ (Infineon BGT60TR13C, 60 GHz raw IQ over SPI) running an owned Rust DSP pipeline (range/Doppler FFT → CFAR → per-role classifier). One node design, per-node role config: `corridor` (walk-past crossing events) / `wet_area` (shower occupancy despite running water). WS:8768 + HTTP:8080 debug/research UI + planned `skopos` HA component (zeroconf). Deliberately NOT an ESP32/module design — vendor radar modules only expose post-DSP summaries (ledger shq-suite-0036/0037). Design in [`skopos/SPEC.md`](skopos/SPEC.md). |

## Reference Docs

| Doc | Description |
|-----|-------------|
| [`docs/actron-local-control.md`](docs/actron-local-control.md) | Actron A/C hardware identity, comms architecture, and the ICUNO-MOD Modbus path for local control (incl. register map + the per-zone-temp limitation) |

## Common Patterns

### Rust Applications (nyx, overwatch, dosa)
- All target **Raspberry Pi 5 ARM64** via `cross` with Podman (not Docker)
- Cross-compile: `cd <app> && ./build-rpi.sh`
- Build output goes to `<app>/build/` for deployment
- **The edge suite has moved out:** `nyx`, `chronos`, `overwatch`, and `argus` are now authoritative in the **Argus repo** (`jordonsc/argus`) — build/deploy them with that repo's `./setup`, not here. The shq-suite copies are secondary (some already diverged: overwatch 0.5.1 vs 0.5.0, argus split out entirely). Deploying them from here can regress the host. Still owned by **this** repo: `dosa`, the firmware (`actron-sniffer`, `somfy-sdn`), `shelly`, and the `dosa`/`centurion`/`actron_mitm_controller`/`somfy_sdn`/`cfa_fire_ban`/`unifi_access_dps` HA components. See ledger shq-suite-0015.
- All use `tokio` async runtime, `tracing` for logging, `serde` for JSON
- Run with `RUST_LOG=info` (or `RUST_LOG=<app>=debug`)
- No test suites — tested manually on hardware

### Argus is the exception — NATIVE x86_64 (atlas), containerised
- Argus runs on **atlas** (x86_64): a plain `cargo build --release` (**no `cross`, no `build-rpi.sh`**), but **packaged as a rootless Podman container** managed by a systemd `--user` Quadlet unit — the same pattern as atlas's `qdrant`/`rag-serve`.
- Deploy with **`argus/deploy-container.sh`** (rsync minimal context → `podman build localhost/argus:latest` on atlas → install `argus.container` Quadlet unit → `systemctl --user restart argus`). Config/seed/secrets bind-mount from `~/.config/argus/` on atlas; the case journal from `~/.local/share/argus/`. Boot-persistent via linger. See `argus/CLAUDE.md` → "Deployment".
- The legacy native path (`./setup argus --build` → `run_cargo_release` → `~/.local/bin/argus` + `argus.service.example`) is **superseded** by the container deploy but still present in the deploy tool.

### Deployment
- Deploy tool is symlinked as `./setup` in project root
- Sensitive config lives in `deploy/config/` (gitignored)
- All services run as **systemd user services** under the `shq` user
- SSH auth via `~/.ssh/jordon.pem` (username `shq` for kiosks/overwatch/dosa, `jordonsc` for HA). The deploy targets/keys live in `deploy/config/deployment/*.yaml` (gitignored; mirrored in `shq-suite-config`)

### Home Assistant Components
- `shq_display`, `overwatch`, `dosa`, `argus` use YAML config (no config flow)
- `centurion` uses HA config flow (UI-driven setup)
- WebSocket integrations use coordinator pattern with reconnection logic

## Hosts

| Host | Role |
|------|------|
| `atlas` (`redacted.host`) | Home Assistant server + RAG; also runs the **Argus** daemon (native x86_64) as a **rootless Podman container** (systemd `--user` Quadlet, like qdrant/rag-serve) |
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
- **Don't diagnose an ESP32-C6 by polling its HTTP server — you will measure your own probe.** On the actron bridge, `GET /stats` every 15 s **quadrupled** the HA `unavailable` flap rate (22 events in 40 min against a 4-8/hour baseline) and drew free heap from 172 k down to 148 k, recovering once polling stopped: `WebServer` on port 80 and `WebSocketsServer` on 8767 share one small lwIP socket pool and one main loop, so HTTP requests starve the WS service (ledger shq-suite-0038). Read `GET /diag` / `/diag.json` **once**, or take the telemetry off the WS push the firmware already sends — both firmwares now instrument themselves (`src/diag.{h,cpp}` in `actron-sniffer` and `somfy-sdn` — **twins, keep them in step**) and the `actron_mitm_controller` / `somfy_sdn` diagnostic sensors record it all continuously in HA without touching the device at all.
- **A blocked TCP socket costs the main loop ~10 s per write, and no timeout you can set will
  shorten it.** `NetworkClient::write()` (Arduino core) retries up to `WIFI_CLIENT_MAX_WRITE_RETRY`
  (10) times around a `select()` bounded by `WIFI_CLIENT_SELECT_TIMEOUT_US` (1 s), so ONE write to
  a peer that stopped reading blocks ~10 s. `WEBSOCKETS_TCP_TIMEOUT` is only re-checked *between*
  write calls; `SO_SNDTIMEO` does nothing because the core's `send()` already uses `MSG_DONTWAIT`;
  and both core constants are **unguarded `#define`s**, so no `-D` overrides them. `broadcastTXT()`
  walks every slot at a header + payload write each, which is how N dead slots became the 50-60 s
  main-loop stalls (all clean multiples of 10 s — ledger shq-suite-0038). The fix is ours, not the
  library's: `src/ws_guard.{h,cpp}` (**twins, keep in step**) poll each socket with a ZERO-timeout
  `select()` before writing, skip any that would block, and drop one that stays unwritable for
  `WS_STALL_REAP_MS`. Never call `broadcastTXT()` directly on these firmwares — use
  `broadcastWritableTXT()`.
- **The firmware reports which AP is serving it — trust that, not the UniFi controller.** Both
  firmwares publish `bssid=`/`roams=` in `/stats` + `/diag` and through the WS health push (HA
  sensors `access_point_bssid` / `ap_roams`). The UniFi controller's client list has been observed
  disagreeing with the station about association (wiki `estate/shq-network.md`), so the station
  wins. Used to test whether the long-running availability flaps followed one AP — **they do not**
  (ledger shq-suite-0040): one radio hosts both the worst and several of the cleanest devices.
- **Don't reap a WS client that is still settling.** `WS_STALL_REAP_MS` (3 s) alone killed sockets
  ~6 s old that had never received a frame — an HA coordinator mid-handshake looks identical to a
  dead one at the socket layer — so HA reconnected into the same trap and the flap rate on two
  controllers *doubled*. `WS_REAP_MIN_AGE_MS` (10 s) now protects a young client. The value must
  stay **under** the 15 s ping interval: that is what keeps a born-stuck socket reaped before
  `enableHeartbeat` can block on it, and it is why raising the stall grace to 10 s instead would
  have been wrong (ledger shq-suite-0038).
- **The write-guard must reap BEFORE `server.loop()`.** `enableHeartbeat`'s ping is emitted inside
  `server.loop()` and writes to the socket directly, bypassing `broadcastWritableTXT()` — so a
  blocked slot still present when the library runs costs the full ~10 s core write regardless of
  the guard. Reap first, and keep `WS_STALL_REAP_MS` under the ping interval (3 s vs 15 s).
- **arduinoWebSockets blocks, and only the Arduino main loop may touch it.** The library's socket
  reads/writes are blocking spin loops bounded by `WEBSOCKETS_TCP_TIMEOUT` (library default
  5000 ms — one zombie client could stall the loop for 10-60 s per pass, which is what drove the
  post-0034 availability flapping; ledger shq-suite-0038). Both firmwares now build with
  `-D WEBSOCKETS_TCP_TIMEOUT=500` (`platformio.ini`) — keep it on any new firmware using this
  library. And never call the WS server from another FreeRTOS task: it has no internal locking,
  and a blocking write from a bus/bridge task stalls that task's real work. Use the dirty-flag
  handoff both firmwares now share (`ws_api::notifyStateChanged` → drained by `ws_api::loop()`).
- **ESP32-C6 `millis()` glitches**: on the TinyC6 boards `millis()` occasionally returns a value **far in the future** (proved by error-ring entries stamped beyond a device's own uptime). Any deadline variable that captures one stops firing until the real clock catches up — that is what wedged the somfy/actron WS state heartbeat and produced months of 40 s-cadence HA `unavailable` flapping (ledger shq-suite-0034). Both firmwares now use `mono::now()` (`src/mono.{h,cpp}`, keep the two copies in step) and **unsigned** elapsed-time comparisons: `(uint32_t)(now - last) >= interval`, never `(int32_t)(...)`. Any new timing code on these boards should follow the same two rules.
