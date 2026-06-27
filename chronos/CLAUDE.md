# Chronos Clock Overlay

> ⚠️ **Authoritative source is the Argus repo — build/deploy from there, not here.**
> Chronos was extracted into the **Argus edge suite** (`jordonsc/argus`, `/mnt/t/Repos/argus/chronos`)
> as part of the Argus work, alongside nyx/overwatch/argus. This shq-suite copy is currently in sync
> (byte-identical) but is **secondary** — make changes and deploys in the Argus repo to avoid divergence
> (see ledger shq-suite-0015 for the Overwatch case where this trap bit).

Tiny Rust binary that draws a fullscreen clock on a kiosk, **above** the Chromium dashboard, as an idle screensaver. Spawned and killed by `nyx` (it is not a service and has no API). Replaces the plain screen-off behaviour on kiosks configured with `idle_mode: clock`.

## How it fits together

`nyx` owns the idle/brightness/touch logic. When a kiosk in `idle_mode: clock` hits its `auto_off_time`, nyx **spawns** `chronos` and holds the backlight at `dim_level` (instead of going to brightness 0). On wake (touch or API), nyx **SIGKILLs** chronos — the layer-shell surface is torn down and the live dashboard is revealed instantly, exactly where the user left it. Chrome is never navigated or reloaded.

Chronos handles **no input**: nyx grabs the evdev touch device while the screensaver is up, so the wake tap is consumed by nyx and never reaches the compositor.

## Why a separate overlay (not a Chrome page)

Swapping Chrome to a clock URL would force an ugly dashboard reload on wake. A separate **wlr-layer-shell overlay surface** sits above the fullscreen Chromium by protocol, so dismissing it is instant and stateless. Verified on labwc (Raspberry Pi OS Bookworm, wlroots 0.19).

## Source Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Wayland layer-shell client (sctk + calloop): creates a fullscreen overlay surface, redraws on the minute via a 1 s timer, exits on SIGKILL |
| `src/render.rs` | Glyph rendering into the ARGB shm buffer (fontdue) — two-line HH/MM, dimmed hours |
| `assets/Rubik-Light.ttf` | Bundled font (OFL, `LICENSE-Rubik.txt`) |

## Appearance

- **Rubik Light**, two lines: HH over MM, centred, portrait.
- **Hours dimmed to 65%** of the foreground (soft grey); minutes full white — a quiet hierarchy via colour, not weight.
- Soft white on black. The look was tuned on-device; the constants live in `render.rs`.

## Tuning env vars (dev only; production uses the baked defaults)

| Var | Default | Effect |
|-----|---------|--------|
| `CHRONOS_FONT` | bundled Rubik Light | Path to an alternative TTF (audition fonts without a rebuild) |
| `CHRONOS_GAP` | `0.55` | Gap between the HH and MM lines, as a fraction of cap height |
| `CHRONOS_HOUR_DIM` | `0.65` | Hours brightness factor (0–1) |

## Build & deploy

```bash
./build-rpi.sh          # ARM64 via cross + Podman → build/chronos
```

Deployed **by the kiosk deployer** alongside the nyx binary to `/home/shq/display/chronos` (nyx resolves it via `current_exe().parent`). `./setup kiosk --build` builds it; a deploy without a built binary just skips it (clock mode is opt-in).

## Rolling it out to a kiosk (runbook)

Enabling the clock on a kiosk is a **runtime** operation, **not** a code/config change — `idle_mode` is per-kiosk state nyx persists to `~/.config/shqd/config.json`, driven by an HA select entity. There is no checked-in list of which kiosks use it (the live source of truth is HA + nyx; the human-readable map is the wiki). To roll out to one or more kiosks:

1. **Identify the kiosk number.** The host→room map is premises-specific and lives privately — in the deploy config (`deploy/config/deployment/kiosk.yaml`, room comments) and the wiki estate notes — not in this public repo. HA friendly names are generic ("SHQ Display Kiosk NN"), so don't rely on them; use the map.
2. **Check prerequisites** (chronos ships with the nyx 1.1.0+ kiosk deploy, so this is usually already satisfied fleet-wide):
   - nyx ≥ 1.1.0: `sensor.shq_display_kiosk_NN_version` (older nyx reports `idle_mode` as `unknown`).
   - chronos binary present: `ssh -i ~/.ssh/<key> <user>@<kiosk-host> 'ls -l /home/shq/display/chronos'` (host/user/key per the deploy config).
   - If either is missing, `./setup kiosk --build` to (re)deploy nyx + chronos first.
3. **Flip the mode** via the HA select (entity id pattern `select.shq_display_kiosk_NN_idle_mode`, options `off`|`clock`):
   ```bash
   ./ha post /api/services/select/select_option \
     '{"entity_id": "select.shq_display_kiosk_NN_idle_mode", "option": "clock"}'
   ```
   The select reads back from nyx's own state, so re-reading it confirms nyx accepted and persisted the change. The clock then appears at that kiosk's next idle `auto_off_time`; a tap dismisses it.

To disable, set the same select back to `off`. After a rollout, **update the wiki kiosk map** so the next session doesn't have to rediscover which kiosks are on the clock.

## Runtime requirements

- A **wlroots-based Wayland compositor** advertising `zwlr_layer_shell_v1` (labwc on the kiosks).
- `WAYLAND_DISPLAY` + `XDG_RUNTIME_DIR` in the environment — nyx passes these through (the `nyx.service` unit exports `WAYLAND_DISPLAY=wayland-0`; nyx also backfills it if absent).

## Gotchas

- **No xkbcommon dependency**: sctk is built with `default-features = false, features = ["calloop"]`. Chronos takes no keyboard input, and dropping xkbcommon keeps the (cross-)build free of that system lib.
- **Wayland via dlopen**: `wayland-client` uses the `dlopen` feature so the cross build needs no libwayland in the target sysroot.
- **fontdue can't pin a variable-font weight** — it renders the default master. Bundle a *static* weight TTF (we ship Rubik Light), not a `Font[wght].ttf` variable font.
