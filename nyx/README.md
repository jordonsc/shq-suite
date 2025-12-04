# Nyx Display Server

A Rust-based WebSocket server for monitoring and controlling Raspberry Pi Touch Display brightness and power state. 

## Features

- **Display Control**: Turn display on/off via brightness control
- **Brightness Management**: Adjust brightness (0-10 scale, 0-100% in 10% increments)
- **Real-time Metrics**: WebSocket broadcasting to all connected clients
- **Touch Event Detection**: Linux evdev-based touch monitoring
- **Auto-Dimming**: Configurable automatic brightness reduction on idle
- **Auto-Off**: Turn off display after extended idle period
- **Touch Wake**: Automatically restore brightness on touch
- **Persistent Configuration**: Saves settings to `~/.config/shqd/config.json`

## Architecture

Built with:
- **Tokio**: Async runtime for concurrent operations
- **Tungstenite**: WebSocket protocol implementation
- **evdev**: Linux input device handling for touch events
- **serde**: JSON serialization for message protocol

## Hardware Support

- Raspberry Pi Touch Display 2 (via I2C at `/sys/class/backlight/10-0045/`)
- Raspberry Pi Touch Display (original) (via `/sys/class/backlight/rpi_backlight/`)
- Auto-detection of backlight device

## Building

### Local Build (x86_64)
```bash
cargo build --release
```

### Cross-Compilation for Raspberry Pi (ARM64)
```bash
# Requires 'cross' tool: cargo install cross
./build-rpi.sh
```

The build output will be in `build/nyx`.

## Installation

The server is deployed via the SHQ deploy tool:

```bash
# From the repository root
./setup kiosk -h kiosk02.myhouse.dev
```

## Usage

### Server
```bash
# Start with default settings (0.0.0.0:8765)
./nyx

# Custom host and port
./nyx --host 0.0.0.0 --port 8765
```

### Logging
Set log level via `RUST_LOG` environment variable:
```bash
RUST_LOG=debug ./nyx
```

## WebSocket Protocol

### Message Format
All messages are JSON over WebSocket.

### Client → Server Commands

```json
// Turn display on/off
{"type": "set_display", "state": true}

// Set brightness (0-10)
{"type": "set_brightness", "brightness": 5}

// Get metrics
{"type": "get_metrics"}

// Configure auto-dimming
{
  "type": "set_auto_dim_config",
  "dim_level": 1,
  "bright_level": 7,
  "auto_dim_time": 30,
  "auto_off_time": 120
}

// Get auto-dim config
{"type": "get_auto_dim_config"}

// Wake display
{"type": "wake"}

// Sleep display
{"type": "sleep"}

// No-op (keepalive)
{"type": "noop"}
```

### Server → Client Responses

```json
// Metrics broadcast
{
  "type": "metrics",
  "display": {
    "display_on": true,
    "brightness": 7
  },
  "auto_dim": {
    "dim_level": 1,
    "bright_level": 7,
    "auto_dim_time": 30,
    "auto_off_time": 120,
    "is_dimmed": false,
    "last_touch_time": 1701619234.5
  }
}

// Command response
{
  "type": "response",
  "success": true,
  "command": "set_brightness"
}

// Error response
{
  "type": "error",
  "message": "Error description"
}
```

## Configuration

Configuration is stored at `~/.config/shqd/config.json`:

```json
{
  "auto_dim": {
    "dim_level": 1,
    "bright_level": 7,
    "auto_dim_time": 0,
    "auto_off_time": 0
  }
}
```

- `dim_level`: Brightness when dimmed (0-255)
- `bright_level`: Brightness when active (0-255)
- `auto_dim_time`: Seconds idle before dimming (0=disabled)
- `auto_off_time`: Seconds idle before turning off (0=disabled)

## Permissions

The server requires access to:

- `/sys/class/backlight/*/` - Backlight control (video group)
- `/dev/input/event*` - Touch events (input group)

Add user to required groups:
```bash
sudo usermod -a -G video,input $USER
```

## Compatibility

This Rust implementation is fully compatible with the original Python client. All WebSocket messages follow the same JSON schema, ensuring seamless interoperability.

## Dependencies

- `tokio` - Async runtime
- `tokio-tungstenite` - WebSocket server
- `serde` / `serde_json` - JSON serialization
- `evdev` - Touch event handling
- `anyhow` / `thiserror` - Error handling
- `tracing` - Structured logging
- `directories` - XDG directory support
