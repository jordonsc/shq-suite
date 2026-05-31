// Decoded Actron controller state — derived from NEO func-03 responses on the RS485 bus.
//
// Pure C++ (no Arduino deps) so it builds for the host test runner alongside bridge.cpp.
// Register encodings come from FINDINGS.md §7 + LOCAL-CONTROL-RECIPES.md. Big-endian throughout.

#pragma once

#include <cstddef>
#include <cstdint>

namespace state {

enum class Mode : uint8_t {
  Off = 0,
  Cool = 1,
  Heat = 2,
  Fan = 3,
  Auto = 4,
  Unknown = 0xFF,
};

enum class FanSpeed : uint8_t {
  Off = 0,
  Low = 1,
  Med = 2,
  High = 3,
  Auto = 4,
  Unknown = 0xFF,
};

constexpr size_t NUM_ZONES = 8;

// Sentinel the NEO writes when a sensor isn't reporting (e.g. unused zone slots, or a paired
// zone sensor that's offline). Anything outside the plausible 0..600 (0..60 °C) range is
// suspect; 0x7530 (= 30000) is the recurring one observed in captures (regs 94–101 for unused
// zones, and many page-2 cells).
constexpr uint16_t SENTINEL_NONE = 0x7530;

struct ZoneState {
  bool enabled = false;
  uint16_t current_temp_raw = SENTINEL_NONE;  // reg 94+i; 0.1 °C; SENTINEL_NONE = no reading
  uint16_t cool_setpoint_raw = 0;             // reg 127+i; 0.1 °C
  uint16_t heat_setpoint_raw = 0;             // reg 135+i; 0.1 °C
};

struct ControllerState {
  // Page-1 fields
  Mode mode = Mode::Unknown;
  FanSpeed fan = FanSpeed::Unknown;
  bool continuous_fan = false;
  uint16_t mode_word_raw = 0;            // reg 10 — keep raw so writes preserve feature bits
  uint16_t fan_word_raw = 0;             // reg 11 — keep raw so writes preserve high byte
  uint16_t main_setpoint_raw = 0;        // reg 12 (active)
  uint16_t cool_main_setpoint_raw = 0;   // reg 55
  uint16_t heat_main_setpoint_raw = 0;   // reg 56
  uint8_t zone_enable_mask = 0;          // reg 85 low byte
  ZoneState zones[NUM_ZONES];

  bool page1_seen = false;
  bool page2_seen = false;
};

// Feed one captured RS485 frame (full bytes: header + data + CRC trailer). If it's a NEO
// page-1 (bytecount 0xF8) or page-2 (bytecount 0xF4) func-03 response with a valid CRC,
// decode into `out` and return true if any tracked field changed vs the prior contents of
// `out`. Returns false for unrecognised / corrupt frames (without mutating `out`).
bool feedNeoFrame(ControllerState& out, const uint8_t* frame, size_t len);

// Helpers for the WS surface — pick the "active" setpoint based on master mode.
// Returns SENTINEL_NONE when no setpoint applies (Off / Fan / Unknown).
// In AUTO master mode, returns the cool-store value as the canonical single-field display
// per the spec; firmware writes update both cool + heat stores under the hood.
uint16_t activeMasterSetpoint(const ControllerState& s);
uint16_t activeZoneSetpoint(const ControllerState& s, size_t zone_idx);

const char* modeName(Mode m);
const char* fanName(FanSpeed f);
Mode parseMode(const char* s);
FanSpeed parseFan(const char* s);

}  // namespace state
