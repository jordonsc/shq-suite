# Chronos

A fullscreen clock overlay for SHQ kiosks — the optional clock screensaver shown instead of turning the screen off.

When a kiosk is set to `idle_mode: clock`, the `nyx` display server spawns Chronos at the idle timeout. Chronos draws a large two-line clock (HH over MM, Rubik Light) on a **wlr-layer-shell overlay surface**, which sits above the fullscreen Chromium dashboard. On wake (a touch), nyx kills Chronos and the live dashboard is revealed instantly — Chrome is never reloaded or navigated.

It is a dumb renderer: no input, no networking, no config file. nyx owns all the idle/brightness/touch logic and grabs the touch device while the screensaver is up, so Chronos never needs to handle the wake tap.

## Build

```bash
./build-rpi.sh          # ARM64 (Raspberry Pi) via cross + Podman → build/chronos
cargo build --release   # local (x86_64), for a quick compile check
```

Deployed alongside `nyx` by the kiosk deploy tool (`./setup kiosk --build`).

## Requirements

- A wlroots-based Wayland compositor with `zwlr_layer_shell_v1` (labwc on Raspberry Pi OS Bookworm).
- `WAYLAND_DISPLAY` + `XDG_RUNTIME_DIR` in the environment (nyx provides these).

See [`CLAUDE.md`](CLAUDE.md) for design notes, the tuning env vars, and gotchas.

## Licence

The bundled Rubik font is under the SIL Open Font License — see `assets/LICENSE-Rubik.txt`.
