# Deploy Tool

Python CLI tool for deploying all SHQ components to their respective Raspberry Pi hosts via SSH/rsync. Entry point is `deploy/src/deploy.py`, symlinked as `./setup` in the project root.

## Usage

```bash
./setup ha                          # Deploy HA components to atlas.shq.sh
./setup kiosk                       # Deploy Nyx to all kiosks
./setup kiosk -h kiosk02.shq.sh    # Deploy to specific kiosk
./setup overwatch --build           # Build + deploy Overwatch
./setup dosa --build                # Build + deploy DOSA
```

The `--build` flag runs `build-rpi.sh` in the relevant project directory before deploying.

## Source Layout

| File | Purpose |
|------|---------|
| `src/deploy.py` | Click CLI entry point with subcommands |
| `src/deploy/base.py` | BaseDeployer — SSH, rsync, remote commands |
| `src/deploy/config.py` | Config dataclasses + YAML loader |
| `src/deploy/kiosk_deployer.py` | Kiosk deployment (Nyx binary, Chromium service, wallpaper) |
| `src/deploy/ha_deployer.py` | HA custom component deployment |
| `src/deploy/overwatch_deployer.py` | Overwatch binary, sounds, config, ALSA |
| `src/deploy/dosa_deployer.py` | DOSA binary, config |
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
│   └── dosa.yaml       # DOSA host + auth
├── app/                # Runtime configs pushed to devices
│   ├── overwatch.yaml  # AWS Polly creds, sounds, voices
│   └── dosa.yaml       # Door params, CNC connection
└── service/            # systemd unit file templates
    ├── kiosk/
    │   ├── kiosk.service   # Chromium (has {dashboard_url} placeholder)
    │   └── nyx.service
    ├── overwatch/
    │   └── overwatch.service
    └── dosa/
        └── dosa.service
```

## What Gets Deployed Where

| Target | What | Destination |
|--------|------|-------------|
| `atlas.shq.sh` | HA custom_components | `/etc/hass/custom_components` |
| `kiosk02-07` | Nyx binary | `/home/shq/display/display/` |
| `kiosk02-07` | Wallpaper, services | `~/.config/systemd/user/` |
| `overwatch.shq.sh` | Binary, sounds, config | `~/overwatch/` |
| `kiosk05.shq.sh` | DOSA binary, config | `~/dosa/` |

## Key Details

- All remote services are **systemd user services** (not system-wide)
- Kiosk deployer templates the dashboard URL from hostname (e.g. `kiosk02.shq.sh` -> `dashboard-kiosks/kiosk02`)
- Uses `loginctl enable-linger` so services persist without an active login session
- SSH key: `~/.ssh/jordon.pem`, username: `shq` (kiosks/overwatch/dosa) or `jordonsc` (HA)
