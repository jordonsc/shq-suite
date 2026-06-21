# Deploy Tool

Python CLI tool for deploying all SHQ components to their respective Raspberry Pi hosts via SSH/rsync. Entry point is `deploy/src/deploy.py`, symlinked as `./setup` in the project root.

## Usage

```bash
./setup ha                          # Deploy HA components + reload config
./setup ha --restart                # Deploy HA components + full service restart
./setup kiosk                       # Deploy Nyx to all kiosks
./setup kiosk -h redacted.host    # Deploy to specific kiosk
./setup overwatch --build           # Build + deploy Overwatch
./setup dosa --build                # Build + deploy DOSA
./setup argus --build               # Build (NATIVE) + deploy Argus to atlas
```

For the ARM64 apps (`kiosk`/nyx, `overwatch`, `dosa`) the `--build` flag runs `build-rpi.sh` (`cross`/Podman) in the relevant project directory before deploying.

**`argus` is the exception — it builds NATIVELY.** Argus runs on **atlas** (x86_64), not a Raspberry Pi, so `./setup argus --build` runs a plain `cargo build --release` in `argus/` (via `run_cargo_release` in `deploy.py`, **not** `run_build_script`/`build-rpi.sh`) and ships `argus/target/release/argus`, not a `cross` `build/` dir. Do NOT add a `build-rpi.sh` to argus or route it through the cross path.

## Source Layout

| File | Purpose |
|------|---------|
| `src/deploy.py` | Click CLI entry point with subcommands |
| `src/deploy/base.py` | BaseDeployer — SSH, rsync, remote commands |
| `src/deploy/config.py` | Config dataclasses + YAML loader |
| `src/deploy/kiosk_deployer.py` | Kiosk deployment (Nyx binary, Chronos clock binary, Chromium service, wallpaper) |
| `src/deploy/ha_deployer.py` | HA custom component deployment |
| `src/deploy/overwatch_deployer.py` | Overwatch binary, sounds, config, ALSA |
| `src/deploy/dosa_deployer.py` | DOSA binary, config |
| `src/deploy/argus_deployer.py` | Argus (native x86_64) binary → `~/.local/bin/argus`, HUD `web/` assets, config, service |
| `assets/pi_splash.png` | Kiosk wallpaper |
| `assets/asound.conf` | ALSA dmix config for Overwatch USB audio |

## Configuration

Config lives in `config/` (gitignored). Structure:

```
config/
├── deployment/          # Per-component deploy targets
│   ├── ha.yaml         # HA server host + auth
│   ├── kiosk.yaml      # Kiosk hosts + dashboard URL template
│   ├── overwatch.yaml  # Overwatch host + auth
│   ├── dosa.yaml       # DOSA host + auth
│   └── argus.yaml      # Argus host (atlas) + auth
├── app/                # Runtime configs pushed to devices
│   ├── overwatch.yaml  # AWS Polly creds, sounds, voices
│   ├── dosa.yaml       # Door params, CNC connection
│   └── argus.yaml      # Argus config.yaml (cameras, entities, loop, offsite, outputs, web/kiosks)
├── ha/                 # HA server config files
│   └── configuration.yaml  # Main HA configuration (modbus, templates, etc)
└── service/            # systemd unit file templates
    ├── kiosk/
    │   ├── kiosk.service   # Chromium (has {dashboard_url} placeholder)
    │   └── nyx.service
    ├── overwatch/
    │   └── overwatch.service
    ├── dosa/
    │   └── dosa.service
    └── argus/
        └── argus.service   # mirror argus/argus.service.example
```

> The `argus.yaml` deploy target, the app `argus.yaml`, and the `argus/argus.service` unit are all under the gitignored `config/` — create them locally (mirror to `shq-suite-config`). `deployment/argus.yaml` follows the `overwatch.yaml`/`dosa.yaml` shape: `auth.username: shq`, `auth.private_key: ~/.ssh/jordon.pem`, `hosts: [<atlas>]`, plus an `argus:` block (all optional — sane defaults: binary `argus/target/release/argus`, `web_path` `argus/web`, `install_path` `.local`, `systemd_service` `argus`). Secrets (`HA_TOKEN`/`ANTHROPIC_API_KEY`) are NOT in the app config — they live in `~/.config/argus/argus.env` on atlas, sourced by the systemd `EnvironmentFile`.

## What Gets Deployed Where

| Target | What | Destination |
|--------|------|-------------|
| `redacted.host` | HA custom_components | `/etc/hass/custom_components` |
| `redacted.host` | configuration.yaml | `/etc/hass/configuration.yaml` |
| `redacted.host` | blueprints | `/etc/hass/blueprints` |
| `kiosk02-10` | Nyx binary | `/home/shq/display/` |
| `kiosk02-10` | Chronos clock binary (if built) | `/home/shq/display/chronos` |
| `kiosk02-10` | Wallpaper, services | `~/.config/systemd/user/` |
| `redacted.host` | Binary, sounds, config | `~/overwatch/` |
| `redacted.host` | DOSA binary, config | `~/dosa/` |
| `atlas` | Argus binary (native x86_64) | `~/.local/bin/argus` |
| `atlas` | Argus HUD `web/` assets + config | `~/.config/argus/web/`, `~/.config/argus/config.yaml` |

## Key Details

- All remote services are **systemd user services** (not system-wide)
- **`argus` is the only NATIVE (non-cross) build** — it targets atlas (x86_64), so it never goes through `cross`/Podman/`build-rpi.sh`. The binary installs to `~/.local/bin/argus` (the unit's `ExecStart=%h/.local/bin/argus`), not an install dir like the Pi apps. It also rsyncs `argus/web/` (the HUD app served from `web.dir`) to `~/.config/argus/web/`.
- Kiosk deployer templates the dashboard URL from hostname (e.g. `redacted.host` -> `dashboard-kiosks/kiosk02`)
- Uses `loginctl enable-linger` so services persist without an active login session
- SSH key: `~/.ssh/jordon.pem`, username: `shq` (kiosks/overwatch/dosa) or `jordonsc` (HA). Manual check, e.g.: `ssh -i ~/.ssh/jordon.pem shq@<kiosk-host>` (real hosts live in the gitignored `config/deployment/*.yaml`). Remote binaries install to `/home/shq/display/` (`nyx`, `chronos`)
- Kiosk `--build` builds **both** `nyx` and `chronos`; the deployer copies the chronos binary alongside nyx (after the `--delete` rsync, so it isn't wiped). The `nyx.service` template exports `WAYLAND_DISPLAY=wayland-0` so nyx can spawn chronos (a Wayland layer-shell client). See `chronos/CLAUDE.md`.
- Kiosk deploy installs `wtype` (`apt-get install -y`, idempotent) as its first step — the Wayland virtual-keyboard CLI used to inject HA credentials over SSH for the first login (the OSK won't show in Chromium kiosk mode). See the README "first HA login" pro-tip.
