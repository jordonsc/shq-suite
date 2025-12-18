# DOSA Door Controller - Home Assistant Custom Component

This custom component integrates the DOSA door controller with Home Assistant, providing real-time door control and monitoring through a WebSocket connection.

## Features

- **Cover Entity**: Control the door with full position control (0-100%)
- **Real-time Updates**: WebSocket connection provides instant status updates
- **Automatic Reconnection**: Built-in reconnection handling for reliable operation
- **Button Entities**:
  - **Home**: Run the homing sequence to calibrate the door position
  - **Zero**: Set the current position as the home position
  - **Clear Alarm**: Clear CNC controller alarms

## Installation

1. Copy the `dosa` folder to your Home Assistant `custom_components` directory:
   ```
   <config>/custom_components/dosa/
   ```

2. Restart Home Assistant

## Configuration

Add the following to your `configuration.yaml`:

```yaml
dosa:
  garage_door:
    host: 192.168.1.100
    port: 8766  # Optional, defaults to 8766
    name: Garage Door  # Optional, defaults to "DOSA <device_id>"
```

You can configure multiple DOSA devices:

```yaml
dosa:
  garage_door:
    host: 192.168.1.100
    name: Garage Door
  workshop_door:
    host: 192.168.1.101
    name: Workshop Door
```

## Entities

For each configured device, the following entities will be created:

### Cover Entity
- **Entity ID**: `cover.<name>_door`
- **Features**:
  - Open/Close door
  - Stop door movement
  - Set specific position (0-100%)
- **Attributes**:
  - `state`: Current door state (closed, open, intermediate, opening, closing, homing, alarm, fault, etc.)
  - `position_mm`: Current position in millimeters
  - `position_percent`: Current position as percentage (0-100)
  - `fault_message`: Error message if in fault state
  - `alarm_code`: Alarm code if in alarm state

### Button Entities
- **Home Button** (`button.<name>_home`): Run homing sequence
- **Zero Button** (`button.<name>_zero`): Set current position as home
- **Clear Alarm Button** (`button.<name>_clear_alarm`): Clear CNC alarms

## Usage Examples

### Automations

Open door when arriving home:
```yaml
automation:
  - alias: "Open garage when arriving"
    trigger:
      - platform: state
        entity_id: person.john
        to: "home"
    action:
      - service: cover.open_cover
        target:
          entity_id: cover.garage_door_door
```

Close door at night:
```yaml
automation:
  - alias: "Close garage at night"
    trigger:
      - platform: time
        at: "22:00:00"
    condition:
      - condition: state
        entity_id: cover.garage_door_door
        state: "open"
    action:
      - service: cover.close_cover
        target:
          entity_id: cover.garage_door_door
```

Set door to 50% open:
```yaml
service: cover.set_cover_position
target:
  entity_id: cover.garage_door_door
data:
  position: 50
```

### Lovelace Card

```yaml
type: entities
entities:
  - entity: cover.garage_door_door
  - entity: button.garage_door_home
  - entity: button.garage_door_zero
  - entity: button.garage_door_clear_alarm
```

## Door States

- **pending**: Door not yet homed (needs initialization)
- **closed**: Door is fully closed
- **open**: Door is fully open
- **intermediate**: Door is at a position between closed and open
- **opening**: Door is currently opening
- **closing**: Door is currently closing
- **homing**: Door is running the homing sequence
- **halting**: Door is stopping movement
- **alarm**: CNC controller is in alarm state (use Clear Alarm button)
- **fault**: System error (check `fault_message` attribute)

## Troubleshooting

### Door not responding
1. Check that the DOSA server is running and accessible
2. Verify the host and port in your configuration
3. Check Home Assistant logs for connection errors

### Door in alarm state
- Use the "Clear Alarm" button to clear CNC controller alarms
- Check the `alarm_code` attribute to identify the specific alarm

### Door in fault state
- Check the `fault_message` attribute for details
- Verify the CNC controller connection
- Restart the DOSA server if needed

### Position not updating
- Ensure the door has been homed using the "Home" button
- Check that the DOSA server is properly connected to the CNC controller

## WebSocket API

The component communicates with the DOSA server using WebSocket messages:

### Commands (Client → Server)
- `{"type": "open"}`: Open the door
- `{"type": "close"}`: Close the door
- `{"type": "move", "percent": 50}`: Move to specific position
- `{"type": "home"}`: Run homing sequence
- `{"type": "zero"}`: Zero at current position
- `{"type": "clear_alarm"}`: Clear CNC alarm
- `{"type": "stop"}`: Emergency stop
- `{"type": "status"}`: Request current status

### Responses (Server → Client)
- Status broadcasts (automatic when state changes)
- Command responses confirming success/failure
- Error messages

## License

Part of the SHQ Suite project.
