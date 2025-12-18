# grblHAL Configuration for DOSA Door Controller

This document describes the required grblHAL settings for the DOSA door controller.

## Required Settings

### Homing Configuration

**$27 - Homing Pull-off Distance (mm)**
- **Purpose**: Distance to back off from the limit switch after homing
- **Recommended**: `3.0` mm
- **Command**: `$27=3.0`
- **Description**: After the homing cycle completes and the limit switch is triggered, grblHAL will automatically move this distance away from the switch. This prevents the switch from staying triggered and allows for normal operation.

### Homing Behavior

**$22 - Homing Cycle Enable**
- **Required**: `1` (enabled)
- **Command**: `$22=1`
- **Description**: Enables the homing cycle. Without this, the `$H` command will not work.

**$23 - Homing Direction Invert**
- **Purpose**: Determines which direction the axis moves during homing
- **Values**: Bit mask for each axis (X=1, Y=2, Z=4, A=8, B=16, C=32)
- **Example**: `$23=0` (home towards minimum/negative) or `$23=1` (home towards maximum/positive for X axis)
- **Description**: Set based on where your limit switch is located.

**$24 - Homing Feed Rate (mm/min)**
- **Recommended**: `500.0` to `1000.0` mm/min
- **Command**: `$24=500.0`
- **Description**: Speed at which the axis moves when searching for the limit switch.

**$25 - Homing Seek Rate (mm/min)**
- **Recommended**: `2000.0` to `4000.0` mm/min
- **Command**: `$25=2000.0`
- **Description**: Initial fast speed to approach the limit switch before slowing down to the feed rate.

**$26 - Homing Switch Debounce (milliseconds)**
- **Recommended**: `250` ms
- **Command**: `$26=250`
- **Description**: Delay to wait after limit switch triggers to ensure it's not noise.

## Motion Settings

**$120 - X-axis Acceleration (mm/sec²)**
- **Recommended**: `200.0` to `500.0` mm/sec²
- **Command**: `$120=300.0`
- **Description**: Acceleration for the door axis. Affects how quickly the door can change speed during stop operations.

**$121-$125** - Y, Z, A, B, C axis acceleration (if using different axis)
- Set appropriately for your chosen axis

## Limit Switch Settings

**$5 - Limit Pins Invert**
- **Values**: Bit mask for each axis
- **Command**: `$5=0` or `$5=1` (depending on switch wiring)
- **Description**: Inverts the logic of the limit switch if needed (normally open vs normally closed).

**$20 - Soft Limits Enable**
- **Recommended**: `0` (disabled for DOSA)
- **Command**: `$20=0`
- **Description**: Soft limits are not needed since DOSA manages position through work coordinates.

**$21 - Hard Limits Enable**
- **Recommended**: `1` (enabled)
- **Command**: `$21=1`
- **Description**: Enables hard limit switches for safety.

## Work Coordinate System

The DOSA controller uses work coordinates (WCS) to track the door position:
- After homing, the controller executes `G92 X0` (or appropriate axis) to set the current position as the closed position
- All subsequent movements are relative to this zero point
- Opening distance is configured in DOSA's `config.yaml`, not in grblHAL

## Quick Setup Commands

For a typical X-axis door setup:

```gcode
$22=1          # Enable homing
$23=0          # Home towards minimum (adjust based on limit switch location)
$24=500        # Homing feed rate
$25=2000       # Homing seek rate
$26=250        # Homing debounce
$27=3.0        # Pull-off distance (IMPORTANT!)
$120=300       # X-axis acceleration
$21=1          # Enable hard limits
$20=0          # Disable soft limits
```

## Verifying Settings

1. Connect to your grblHAL controller
2. Send `$$` to view all settings
3. Verify the above settings match your configuration
4. Test homing with `$H` before running DOSA

## Troubleshooting

**Homing fails immediately**
- Check $22 is set to 1 (homing enabled)
- Verify limit switch is wired and working
- Check $5 if switch logic needs inverting

**Homing completes but switch stays triggered**
- Increase $27 (pull-off distance)
- Check $23 to ensure homing in correct direction

**Door moves too fast/slow after homing**
- Adjust DOSA's `open_speed` and `close_speed` in config.yaml
- These are independent from grblHAL homing speeds

**Alarm triggered during movement**
- Check that hard limits ($21) are not triggering during normal operation
- Ensure door doesn't exceed the open_distance configured in DOSA

## Reference

For complete grblHAL settings documentation, see:
https://github.com/grblHAL/core/wiki/Grbl-Settings
