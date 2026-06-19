# Nyx Display Server

Rust application that controls wall display kiosks. Manages backlight brightness, auto-dim/sleep, touch wake detection, and Chrome navigation via CDP. Exposes a WebSocket API on port 8765.

## Source Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point — inits config, display, touch, auto-dim, WebSocket server |
| `src/websocket.rs` | WebSocket server — handles all client commands, broadcasts metrics |
| `src/messages.rs` | JSON message types (ClientMessage/ServerMessage enums) |
| `src/display.rs` | sysfs backlight control — reads/writes `/sys/class/backlight/*/brightness` |
| `src/touch.rs` | evdev touch detection — grab/ungrab for sleep mode, idle tracking |
| `src/auto_dim.rs` | Auto-dim logic — 25ms check loop, dim/bright/off states; spawns/kills the Chronos clock overlay in `idle_mode: clock`. Holds a `pinned_awake` `AtomicBool` (`set_pinned_awake`): while set the idle loop early-returns (never dims/blanks/spawns the clock); cleared = normal idle resumes. Set/cleared via `navigate{keep_awake}` |
| `src/cdp.rs` | Chrome DevTools Protocol — raw HTTP + WebSocket for navigation |
| `src/config.rs` | Persistent JSON config at `~/.config/shqd/config.json` |

## WebSocket API (port 8765)

### Client -> Server
- `set_display { state: bool }` — on/off
- `set_brightness { brightness: 0-255 }` — direct brightness
- `wake` / `sleep` — explicit wake/sleep
- `navigate { url, wake?, keep_awake? }` — Chrome navigation via CDP. `wake: true` (optional, default no) runs the full `wake()` (kill the Chronos overlay + restore `bright_level` + ungrab touch) BEFORE the navigate, so the new page is visible on a sleeping/`clock` kiosk. `keep_awake: true` PINS the display awake — the idle/auto-dim loop won't dim, blank, or spawn the clock until released; `keep_awake: false` releases the pin (normal idle resumes); omitting it leaves the pin unchanged. Both fields are `#[serde(default)]` (`Option`) so old callers that send only `{url}` are unaffected. (Used by Argus's kiosk takeover: `wake:true` on every alarm-mode navigate, `keep_awake:true` only while an alarm is *active* (Triggered/live case), `keep_awake:false` for arming/armed/authorised standby and on the return to the dashboard.)
- `get_url` — current Chrome URL
- `get_metrics` — request state broadcast
- `set_auto_dim_config { dim_level, bright_level, auto_dim_time, auto_off_time, idle_mode? }`
- `get_auto_dim_config`
- `noop` — keepalive

`idle_mode` is `"off"` (default) or `"clock"`. It is `#[serde(default)]` so old clients that omit it keep working — **but any client that sends `set_auto_dim_config` must include the current `idle_mode`, or it resets to `off`** (the HA Number/Select entities all preserve it).

### Server -> Client
- `metrics { version, display, auto_dim, url }` — periodic + on-change broadcast
- `response { success, command, config?, url? }` — command ack
- `error { message }` — error

## Display Backlight

Auto-detects device in priority order:
1. RPi Touch Display 2: `/sys/class/backlight/10-0045/`
2. Original RPi Touch: `/sys/class/backlight/rpi_backlight/`
3. Any available device in `/sys/class/backlight/`

Brightness 0-255 maps to device's native range. Caches last non-zero brightness for wake restore (default 178 / ~70%).

## CDP Integration

Talks to Chromium's `--remote-debugging-port=9222`:
1. HTTP GET `127.0.0.1:9222/json` with `Host: 127.0.0.1:9222`
2. Find page target, extract `webSocketDebuggerUrl`
3. WebSocket `Page.navigate` command

**Critical**: Must include port in Host header. Must parse Content-Length and read_exact (not read_to_end).

## Clock screensaver (`idle_mode: clock`)

By default a kiosk blanks the backlight at `auto_off_time` (`idle_mode: off`). A kiosk set to `idle_mode: clock` instead shows the **Chronos** clock overlay (separate `chronos/` crate) and holds the backlight at `dim_level`:

- **Auto-off in clock mode** (`auto_dim.rs`): spawn `chronos` (resolved next to the nyx binary via `current_exe().parent`), set brightness to `dim_level`, grab touch. The clock uses `dim_level` as its brightness — there is no separate clock-brightness setting.
- **Wake** (touch / `wake` / `set_display true`): SIGKILL chronos (surface torn down → live dashboard revealed instantly, no Chrome reload), restore `bright_level`, ungrab. The existing evdev grab means the wake tap is consumed and never reaches Chrome.
- **Explicit `sleep` / `set_brightness 0`** always blanks (kills the clock too) — the clock is only the *idle* screensaver.
- On startup nyx `pkill -x chronos` to clear any overlay orphaned by a hard restart; spawned children are also `kill_on_drop`.
- **Pinned awake** (`navigate{keep_awake:true}`): an `AtomicBool` the idle loop checks first — while set it never dims, blanks, or spawns the clock (used by Argus to force the screen on during a live alarm). `navigate{wake:true}` does a one-shot wake (kills the clock, restores brightness) but does NOT pin; combine with `keep_awake:true` to also hold it. `keep_awake:false` releases the pin and normal idle resumes. The pin does not survive a nyx restart (resets to false).

Chronos is a Wayland layer-shell client, so nyx must run with `WAYLAND_DISPLAY` set (the `nyx.service` unit exports `wayland-0`; nyx also backfills it on spawn if absent). Requires the `process` tokio feature.

## Building

```bash
cargo build --release          # Local (x86_64)
./build-rpi.sh                 # ARM64 for RPi (uses cross + Podman)
./build-rpi.sh --debug         # Debug build for RPi
```

Output: `build/nyx`

## Runtime Requirements

User must be in `video` and `input` groups for sysfs backlight and evdev access.
