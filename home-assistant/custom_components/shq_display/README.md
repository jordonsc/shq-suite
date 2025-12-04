# SHQ Display Integration for Home Assistant

This integration allows you to control SHQ Display devices from Home Assistant.

## Features

- **Light Entity**: Control display brightness and on/off state (uses wake/sleep commands)
- **Number Entities**: Configure auto-dim and auto-off timers

## Installation

1. Copy the `shq_display` folder to your `custom_components` directory in Home Assistant
2. Add configuration to your `configuration.yaml`
3. Restart Home Assistant

## Configuration

Add the following to your `configuration.yaml`:

```yaml
shq_display:
  # First display
  display1:
    host: 192.168.1.100
    port: 8765  # Optional, defaults to 8765
    name: "Living Room Display"

  # Second display (if you have multiple)
  display2:
    host: 192.168.1.101
    name: "Bedroom Display"
```

## Entities Created

For each display configured, the following entities will be created:

### Light
- `light.living_room_display` - Control brightness (0-100%) and on/off state
  - **Turn On**: Wakes display to bright level
  - **Turn Off**: Sleeps display (brightness 0)
  - **Set Brightness**: Sets specific brightness level

### Number Inputs
- `number.living_room_display_dim_level` - Brightness level when dimmed (0-10)
- `number.living_room_display_bright_level` - Brightness level when active (0-10)
- `number.living_room_display_dim_time` - Seconds before auto-dimming (0 = disabled)
- `number.living_room_display_off_time` - Seconds before turning off (0 = disabled)

## Usage Examples

### Automation: Turn on display in the morning

```yaml
automation:
  - alias: "Wake living room display"
    trigger:
      - platform: time
        at: "07:00:00"
    action:
      - service: button.press
        target:
          entity_id: button.living_room_display_wake
```

### Automation: Dim display at night

```yaml
automation:
  - alias: "Dim display at night"
    trigger:
      - platform: time
        at: "22:00:00"
    action:
      - service: light.turn_on
        target:
          entity_id: light.living_room_display
        data:
          brightness: 25  # 10% brightness
```

### Script: Configure auto-dim settings

```yaml
script:
  configure_display_timers:
    sequence:
      - service: number.set_value
        target:
          entity_id: number.living_room_display_dim_time
        data:
          value: 30  # Dim after 30 seconds
      - service: number.set_value
        target:
          entity_id: number.living_room_display_off_time
        data:
          value: 60  # Turn off after 60 seconds
```

## Notes

- Brightness in Home Assistant (0-100%) is automatically converted to the display's native scale (0-10)
- The integration connects to the SHQ Display server via WebSocket
- Each command establishes a connection, sends the command, and disconnects
- The light entity polls for updates when state is requested
