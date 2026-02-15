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
| `src/auto_dim.rs` | Auto-dim logic — 25ms check loop, dim/bright/off states |
| `src/cdp.rs` | Chrome DevTools Protocol — raw HTTP + WebSocket for navigation |
| `src/config.rs` | Persistent JSON config at `~/.config/shqd/config.json` |

## WebSocket API (port 8765)

### Client -> Server
- `set_display { state: bool }` — on/off
- `set_brightness { brightness: 0-255 }` — direct brightness
- `wake` / `sleep` — explicit wake/sleep
- `navigate { url }` — Chrome navigation via CDP
- `get_url` — current Chrome URL
- `get_metrics` — request state broadcast
- `set_auto_dim_config { dim_level, bright_level, auto_dim_time, auto_off_time }`
- `get_auto_dim_config`
- `noop` — keepalive

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

## Building

```bash
cargo build --release          # Local (x86_64)
./build-rpi.sh                 # ARM64 for RPi (uses cross + Podman)
./build-rpi.sh --debug         # Debug build for RPi
```

Output: `build/nyx`

## Runtime Requirements

User must be in `video` and `input` groups for sysfs backlight and evdev access.
