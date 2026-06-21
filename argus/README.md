# Argus

> AI-powered alarm assessment layer for the SHQ home-automation suite.

When the house alarm (`alarm_control_panel.shq_alarm`) fires, **Argus** opens a
case and runs a multi-camera + telemetry assessment loop: a frontier multimodal
LLM (Anthropic Claude) builds an evolving structured **`CaseState`** — a
real-time *who / what / where* record — fed by a private premises seed. Sonnet
runs the fast live loop; Opus does forensic identification on the best stills.

Argus **consumes** the existing Home Assistant alarm and the 11 UniFi Protect
cameras — it is the intelligence layer on top, not a replacement. It is a native
**x86_64** daemon that runs on **atlas** (the HA + RAG host).

Phases 1–2 are complete: *trigger → multi-camera loop → structured `CaseState`*
(journalled to disk + broadcast in-process). The remaining outputs (real-time
offsite resilience, Overwatch intimidation voice, PagerDuty dispatch, kiosk HUD,
HA component) are delivered across later phases — see
[`../specs/argus/`](../specs/argus/) (read `00-master.md` first).

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

# 4. Test the full pipeline without arming the house (one tick → print CaseState)
./target/release/argus --once

# 5. Run as a daemon (watches the alarm, runs the loop while triggered)
./target/release/argus
```

A second `--once` run within ~5 minutes shows `cache_read > 0` in the per-tick
token-usage line — the premises seed is served from the prompt cache (Sonnet
caches a ≥2048-token seed; **Opus needs ≥4096 tokens**, so the real seed should
exceed that). Cases are written under `~/.local/share/argus/cases/`.

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
