# Overwatch Voice Control

Home Assistant custom component for controlling the Overwatch voice server via gRPC.

## Features

- **Set Alarm**: Enable or disable alarms with optional volume control
- **Verbalise**: Text-to-speech synthesis with optional notification tones and voice selection

## Installation

1. Copy this directory to your Home Assistant `custom_components` folder:
   ```
   custom_components/overwatch/
   ```

2. Install Python dependencies (if not already installed in your HA environment):
   ```bash
   pip install grpcio grpcio-tools protobuf
   ```

3. Generate the proto files:
   ```bash
   cd custom_components/overwatch/proto
   python3 -m grpc_tools.protoc -I. --python_out=. --grpc_python_out=. voice.proto
   ```

4. Add configuration to your `configuration.yaml`:
   ```yaml
   overwatch:
     host: "192.168.1.100"  # IP address of your voice server
     port: 50051            # gRPC port (default: 50051)
   ```

5. Restart Home Assistant

## Services

### `overwatch.set_alarm`

Enable or disable an alarm.

**Parameters:**
- `alarm_id` (required): ID of the alarm (e.g., "fire")
- `enabled` (required): True to start, False to stop
- `volume` (optional): Volume level 0.0-2.0

**Example:**
```yaml
service: overwatch.set_alarm
data:
  alarm_id: "fire"
  enabled: true
  volume: 1.0
```

### `overwatch.verbalise`

Synthesize and play text as speech.

**Parameters:**
- `text` (required): Text to speak
- `notification_tone_id` (optional): Tone to play before speech (e.g., "notify")
- `voice_id` (optional): Voice to use (e.g., "Amy", "Brian")
- `volume` (optional): Volume level 0.0-2.0

**Example:**
```yaml
service: overwatch.verbalise
data:
  text: "Hello, this is a test message"
  notification_tone_id: "notify"
  voice_id: "Amy"
  volume: 1.0
```

## Automation Examples

### Morning Alarm
```yaml
automation:
  - alias: "Morning Alarm"
    trigger:
      - platform: time
        at: "07:00:00"
    action:
      - service: overwatch.verbalise
        data:
          text: "Good morning! Time to wake up!"
          notification_tone_id: "notify"
          voice_id: "Amy"
```

### Fire Alarm
```yaml
automation:
  - alias: "Fire Alarm Trigger"
    trigger:
      - platform: state
        entity_id: binary_sensor.smoke_detector
        to: "on"
    action:
      - service: overwatch.set_alarm
        data:
          alarm_id: "fire"
          enabled: true
          volume: 2.0
      - service: overwatch.verbalise
        data:
          text: "Fire alarm activated. Please evacuate immediately."
          voice_id: "Amy"
          volume: 2.0
```

### Stop Alarm
```yaml
automation:
  - alias: "Stop Fire Alarm"
    trigger:
      - platform: state
        entity_id: binary_sensor.smoke_detector
        to: "off"
        for:
          minutes: 2
    action:
      - service: overwatch.set_alarm
        data:
          alarm_id: "fire"
          enabled: false
```

## Troubleshooting

### Proto files not found
If you see an error about missing proto files, make sure you've generated them:
```bash
cd custom_components/overwatch/proto
python3 -m grpc_tools.protoc -I. --python_out=. --grpc_python_out=. voice.proto
```

### Connection refused
- Verify the voice server is running
- Check the host and port in your configuration.yaml
- Ensure the gRPC port (default 50051) is accessible from Home Assistant

### View logs
Check Home Assistant logs for Overwatch-related messages:
```bash
grep -i overwatch home-assistant.log
```

## Development

The component follows Home Assistant's custom component structure:
- `__init__.py`: Component setup and service registration
- `client.py`: gRPC client for voice server communication
- `const.py`: Constants and configuration keys
- `services.yaml`: Service definitions for UI
- `manifest.json`: Component metadata and dependencies
- `proto/`: Protocol buffer definitions and generated code
