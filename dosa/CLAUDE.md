# DOSA — Door Opening Sensor Automation

Rust application controlling an automated door via a grblHAL CNC controller and linear actuator. Exposes a WebSocket API on port 8766.

## Source Layout

| File | Purpose |
|------|---------|
| `src/main.rs` | Entry point — loads config, inits CNC connection, starts WebSocket |
| `src/cnc.rs` | CNC controller — serial/TCP connection, G-code commands, status parsing |
| `src/door.rs` | Door controller — state machine, open/close/stop/home/jog logic |
| `src/messages.rs` | WebSocket message types (ClientMessage/ServerMessage) |
| `src/websocket.rs` | WebSocket server — command handling, status broadcasts |
| `src/config.rs` | YAML config parsing |

## WebSocket API (port 8766)

### Client -> Server
- `open` — open the door fully
- `close` — close the door
- `stop` — emergency stop (feed hold + queue flush)
- `move { percent: 0-100 }` — move to position percentage
- `jog { distance, feed_rate? }` — relative movement in mm
- `home` — run homing sequence (finds limit switch)
- `zero` — set current position as home (0mm)
- `clear_alarm` — clear CNC alarm state
- `status` — request current status
- `get_cnc_settings` / `get_cnc_setting` / `set_cnc_setting` — grblHAL settings
- `noop` — keepalive

### Server -> Client
- `status { state, position_mm, position_percent, fault_message?, alarm_code?, alarm_description? }`
- `response { success, command, data?, error? }`
- `cnc_settings { settings }` / `cnc_setting { name, value }`

## Door States

`Pending` -> `Homing` -> `Closed` <-> `Opening`/`Closing` <-> `Open`/`Intermediate`

Also: `Halting`, `Fault`, `Alarm`

## CNC Connection

Supports both:
- **Serial**: `/dev/ttyUSB0` at 115200 baud (default)
- **TCP**: e.g. `192.168.1.65:23`

Uses grblHAL protocol: `?` for status, `!` for feed hold, `0x18` for queue flush, `$H` for homing. Sends G-code for movement (`G90 G1 X{pos} F{speed}`).

## Configuration (`config.yaml`)

```yaml
door:
  open_distance: 520.0      # mm
  open_speed: 6000.0         # mm/min
  close_speed: 6000.0        # mm/min
  cnc_axis: "X"
  limit_offset: 3.0          # mm back from limit switch after homing
  open_direction: right       # "left" or "right"
  auto_home: true
  cnc_connection:
    type: serial              # or "tcp"
    port: "/dev/ttyUSB0"
    baud_rate: 115200
websocket:
  host: 0.0.0.0
  port: 8766
```

## Key Behaviours

- **Stop**: Uses feed hold (`!`) to decelerate safely, polls for `Hold:0`, then queue flush
- **Auto-reconnect**: CNC connection retries on failure with `execute_with_reconnect()`
- **Position tracking**: Parses grblHAL status responses (`<Idle|MPos:X,Y,Z|...>`)
- **Homing**: Required before open/close. Moves to limit switch, backs off by `limit_offset`

## Building

```bash
cargo build --release          # Local
./build-rpi.sh                 # ARM64 for RPi
```

Output: `build/dosa`

Runs on `kiosk05.shq.sh` alongside a kiosk (both under `shq` user).
