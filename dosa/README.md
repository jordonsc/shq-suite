# DOSA (Door Opening Sensor Automation)

DOSA is a Rust-based door automation system that controls automated doors via a grblHAL CNC controller. It provides WebSocket-based real-time control and monitoring of door operations.

## Features

- WebSocket API for real-time door control and status updates
- Support for both TCP and Serial connections to grblHAL CNC controllers
- Configurable door parameters (speeds, distances, axis)
- Graceful motion handling (reversing mid-operation)
- Automatic position monitoring and status updates
- YAML-based persistent configuration
- Designed for Raspberry Pi deployment

## Building

### Local Development (Linux/WSL)

```bash
cargo build
cargo run
```

### Raspberry Pi Deployment

```bash
./build-rpi.sh        # Release build
./build-rpi.sh -d     # Debug build
```

The compiled binary will be in `build/dosa`.

## Configuration

Configuration is stored in `~/.config/dosa/config.yaml`. The application creates a default configuration on first run.

See `config.example.yaml` for a complete example with both TCP and Serial connection options.

### Default Configuration

- Open distance: 1000mm
- Open speed: 6000mm/min
- Close speed: 4000mm/min
- CNC axis: X
- Limit offset: 3mm
- Stop delay: 1000ms
- Open direction: right

### Connection Types

#### TCP Connection (Network)
```yaml
cnc_connection:
  type: tcp
  host: "192.168.1.100"
  port: 23
```

#### Serial Connection (USB)
```yaml
cnc_connection:
  type: serial
  port: "/dev/ttyUSB0"  # Linux: /dev/ttyUSB0, /dev/ttyACM0, etc.
  # port: "COM3"         # Windows: COM3, COM4, etc.
  baud_rate: 115200
```

## Running

```bash
# Default: Listen on 0.0.0.0:8766
./dosa

# Custom host and port
./dosa --host 127.0.0.1 --port 9000
```

## grblHAL Controller Configuration

Before using DOSA, configure your grblHAL controller's acceleration settings. These control how quickly the door can accelerate and decelerate (the same value is used for both).

### Important Settings:

- **`$120`** - X-axis acceleration (mm/sec²)
- **`$121`** - Y-axis acceleration (mm/sec²)
- **`$122`** - Z-axis acceleration (mm/sec²)
- **`$123`** - A-axis acceleration (mm/sec²)
- **`$124`** - B-axis acceleration (mm/sec²)
- **`$125`** - C-axis acceleration (mm/sec²)

### Recommended Values:

The acceleration setting affects how quickly the door stops when executing direction reversals or emergency stops:

- **Low (500 mm/sec²)** - Gentle, smooth stopping. Requires longer `stop_delay_ms`.
- **Medium (1000 mm/sec²)** - Balanced stopping. Works with default `stop_delay_ms: 1000`.
- **High (2000+ mm/sec²)** - Aggressive stopping. Can reduce `stop_delay_ms` but may stress mechanics.

**Example Configuration:**
```
$120=1000.000   # X-axis acceleration for door on X-axis
$130=1500.000   # X-axis max travel (should be >= your open_distance)
```

### To Configure:

Connect to your grblHAL controller via serial/TCP and send:
```
$$              # View all current settings
$120=1000       # Set X-axis acceleration to 1000 mm/sec²
```

**Important:** Set acceleration based on your door's mechanical limits. Too high risks:
- Mechanical stress on door hardware
- Motor stalling
- Lost steps (position errors)

Test your settings carefully and adjust `stop_delay_ms` in the DOSA config to match your controller's deceleration characteristics.

### Automatic Validation

DOSA automatically validates your configuration on startup. It:
1. Queries the controller's acceleration setting for your configured axis
2. Calculates the minimum deceleration time from your max speed
3. Verifies that `stop_delay_ms` is sufficient (with 20% safety margin)

If validation fails, DOSA will refuse to start and provide specific recommendations:
```
Error: stop_delay_ms (500 ms) is too short for safe deceleration!
Maximum speed: 6000 mm/min (100.0 mm/sec)
Acceleration: 1000 mm/sec²
Minimum deceleration time: 100 ms
Recommended stop_delay_ms: 120 ms (with 20% safety margin)

Either:
1. Increase stop_delay_ms to at least 120 ms in your config, or
2. Reduce open_speed/close_speed, or
3. Increase controller acceleration setting $120
```

This ensures your door can safely decelerate before reversing direction.

## Alarm Monitoring

DOSA continuously monitors the CNC controller for alarm states. When an alarm is detected:

1. `is_alarm` is set to `true` in the status
2. `alarm_code` contains the alarm code if provided by the controller
3. All door operations (open, close, home, zero) are blocked until the alarm is cleared
4. The system does NOT enter fault state for alarms (alarms are operational issues, not connection issues)

### Common Alarm Codes

grblHAL alarm codes indicate various error conditions:
- **Alarm 1**: Hard limit triggered
- **Alarm 2**: Soft limit triggered
- **Alarm 3**: Abort during homing cycle
- **Alarm 4**: Probe fail (not applicable for door control)
- **Alarm 5**: Homing fail (could not find limit switch)
- **Alarm 6**: Homing fail (switch not cleared)
- **Alarm 7**: Homing fail (pull-off travel exceeded)
- **Alarm 8**: Homing fail (switch not found during locate phase)

### Clearing Alarms

After resolving the cause of an alarm (e.g., moving the door away from a limit switch), use the `clear_alarm` command to reset the controller and clear the fault state.

## WebSocket API

Connect to `ws://<host>:<port>` (default: `ws://localhost:8766`)

### Client Messages (Commands)

#### Open Door
```json
{"type": "open"}
```

#### Close Door
```json
{"type": "close"}
```

#### Home Door
```json
{"type": "home"}
```
Moves the door to the limit switch, then backs off by `limit_offset` and sets that position as the closed (home) position.

#### Zero Door
```json
{"type": "zero"}
```
Sets the current position as the home (closed) position without performing a homing sequence. Useful when the door is already at the desired closed position and you want to zero it without moving to the limit switch.

#### Clear Alarm
```json
{"type": "clear_alarm"}
```
Clears a CNC alarm state by sending the `$X` unlock command to the grblHAL controller. If the system is in fault state due to an alarm, this will also clear the fault state. Use this after resolving the cause of the alarm (e.g., limit switch hit, homing failure).

#### Get Status
```json
{"type": "get_status"}
```

#### Set Configuration
```json
{
  "type": "set_config",
  "open_distance": 1200.0,
  "open_speed": 7000.0,
  "close_speed": 5000.0,
  "cnc_axis": "Y",
  "limit_offset": 5.0,
  "stop_delay_ms": 1500,
  "open_direction": "right"
}
```
All fields are optional. Only provided fields will be updated.

**Open Direction:**
- `"right"`: Door opens in the positive direction (e.g., 0mm → +1000mm)
- `"left"`: Door opens in the negative direction (e.g., 0mm → -1000mm)

#### Get Configuration
```json
{"type": "get_config"}
```

#### Emergency Stop
```json
{"type": "stop"}
```

#### Keep-Alive
```json
{"type": "noop"}
```

### Server Messages (Responses)

#### Status Update
Sent automatically when state changes and in response to `get_status`:
```json
{
  "type": "status",
  "version": "1.0.0",
  "door": {
    "state": "closed",         // "pending", "closed", "open", "opening", "closing", "homing", "alarm", "fault"
    "position_mm": 0.0,        // Position relative to home (0 = closed), or 0 if not yet homed
    "fault_message": null,     // Error message if in fault state
    "alarm_code": null         // Alarm code if in alarm state (e.g., "1", "2")
  }
}
```

**State Values:**
- `pending`: Door has not been homed yet (needs initialization)
- `closed`: Door is at the closed (home) position
- `open`: Door is at the open position
- `opening`: Door is currently opening
- `closing`: Door is currently closing
- `homing`: Door is performing homing sequence
- `alarm`: CNC controller is in alarm state (must be cleared)
- `fault`: System is in fault state (connection error)

#### Command Response
```json
{
  "type": "response",
  "success": true,
  "command": "open",
  "config": null  // Only present for get_config response
}
```

#### Error
```json
{
  "type": "error",
  "message": "Failed to open door: Door must be homed before opening."
}
```

## Operation Flow

1. **First Run**: Establish the home position using either:
   - `home` command - Moves the door to the limit switch and establishes the closed position
   - `zero` command - Sets the current position as home without moving (when door is already closed)
2. **Open**: Send `open` command to move the door to the configured open position.
3. **Close**: Send `close` command to return the door to the closed position.
4. **Monitoring**: Status updates are broadcast when the door state changes.

### Graceful Motion Handling

If the door is closing and an `open` command is received (or vice versa), DOSA will:
1. Send an emergency stop to the CNC
2. Wait briefly for the motion to halt
3. Immediately execute the new command

This ensures smooth reversals without mechanical stress.

## grblHAL Commands Used

- `$H<axis>` - Home the specified axis
- `G90 G1 <axis><pos>F<speed>` - Absolute positioning move
- `G92 X0 Y0 Z0` - Reset position counters
- `?` - Status query
- `0x21` (!) - Feed hold (pause)
- `0x7E` (~) - Cycle start (resume)
- `0x18` (Ctrl-X) - Soft reset

## Logging

Set the `RUST_LOG` environment variable to control logging:

```bash
RUST_LOG=dosa=debug ./dosa   # Debug logging
RUST_LOG=dosa=info ./dosa    # Info logging (default)
```

## Dependencies

- tokio - Async runtime
- tokio-tungstenite - WebSocket support
- tokio-serial - Serial port communication
- serde/serde_json/serde_yaml - Serialization
- anyhow/thiserror - Error handling
- tracing/tracing-subscriber - Logging

## License

Part of the SHQ Suite home automation tools.
