# Argus

> AI-powered alarm assessment layer for the SHQ home-automation suite.

When the house alarm (`alarm_control_panel.shq_alarm`) fires, **Argus** pulls a
camera still, feeds it to a frontier multimodal LLM (Anthropic Claude) along with
a private premises seed, and produces a real-time *who / what / where* assessment.

Argus **consumes** the existing Home Assistant alarm and the 11 UniFi Protect
cameras — it is the intelligence layer on top, not a replacement. It is a native
**x86_64** daemon that runs on **atlas** (the HA + RAG host).

This is **Phase 1** — the smallest end-to-end slice: *trigger → still →
assessment*, logged as text. The full system (multi-camera loop, tiered
Sonnet/Opus, structured `CaseState`, real-time offsite resilience, Overwatch
intimidation voice, PagerDuty dispatch, kiosk HUD, HA component) is delivered
across later phases — see [`../specs/argus/`](../specs/argus/) (read
`00-master.md` first).

## Quick start

```bash
# 1. Build (native — runs on/for atlas; NOT a cross/RPi build)
cargo build --release

# 2. Configure
mkdir -p ~/.config/argus
cp config.yaml.example ~/.config/argus/config.yaml   # edit cameras/entities
cp seed.example.md     ~/.config/argus/seed.md        # the real seed is private

# 3. Provide secrets via the environment (never in config.yaml)
export HA_TOKEN=...            # HA UI → profile → long-lived tokens
export ANTHROPIC_API_KEY=...

# 4. Test the HA + Anthropic path without arming the house
./target/release/argus --once --camera camera.garage_camera_high_resolution_channel

# 5. Run as a daemon (watches the alarm)
./target/release/argus
```

A second `--once` run within ~5 minutes should report
`cache_read > 0` in the token-usage line — the premises seed is served from the
prompt cache.

## Configuration

See [`config.yaml.example`](config.yaml.example). `${VAR}` placeholders are
expanded from the environment at load, so secrets stay out of the file. The real
`config.yaml` / `seed.md` are gitignored and mirrored to `shq-suite-config`.

## Deployment

Runs as a systemd **user** service under the `shq` user on atlas — see
[`argus.service.example`](argus.service.example). The deploy-tool target and HA
integration land in Phase 5.

## Documentation

- [`CLAUDE.md`](CLAUDE.md) — component reference (source layout, API call shape, gotchas)
- [`../specs/argus/`](../specs/argus/) — the phased design specs
