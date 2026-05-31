#include "state.h"
#include "bridge.h"

#include <cstring>

namespace state {

namespace {

constexpr uint8_t ADDR_NEO = 0x66;
constexpr uint8_t FUNC_READ_HOLDING = 0x03;
constexpr uint8_t BYTECOUNT_PAGE1 = 0xF8;  // 248 data bytes, regs 2..125
constexpr uint8_t BYTECOUNT_PAGE2 = 0xF4;  // 244 data bytes, regs 126..247
constexpr uint16_t PAGE1_START = 2;
constexpr uint16_t PAGE2_START = 126;

inline uint16_t readReg(const uint8_t* data, uint16_t reg, uint16_t start_addr) {
  size_t off = (size_t)(reg - start_addr) * 2;
  return (uint16_t)((uint16_t)data[off] << 8) | (uint16_t)data[off + 1];
}

Mode decodeMode(uint16_t word) {
  switch (word & 0x07) {
    case 0: return Mode::Off;
    case 1: return Mode::Cool;
    case 2: return Mode::Heat;
    case 3: return Mode::Fan;
    case 4: return Mode::Auto;
    default: return Mode::Unknown;
  }
}

FanSpeed decodeFan(uint16_t word) {
  switch (word & 0x0F) {
    case 0: return FanSpeed::Off;
    case 1: return FanSpeed::Low;
    case 2: return FanSpeed::Med;
    case 3: return FanSpeed::High;
    case 4: return FanSpeed::Auto;
    default: return FanSpeed::Unknown;
  }
}

bool decodePage1(ControllerState& s, const uint8_t* data) {
  bool changed = !s.page1_seen;

  uint16_t mode_word = readReg(data, 10, PAGE1_START);
  if (mode_word != s.mode_word_raw) { s.mode_word_raw = mode_word; changed = true; }
  Mode m = decodeMode(mode_word);
  if (m != s.mode) { s.mode = m; changed = true; }

  uint16_t fan_word = readReg(data, 11, PAGE1_START);
  if (fan_word != s.fan_word_raw) { s.fan_word_raw = fan_word; changed = true; }
  FanSpeed f = decodeFan(fan_word);
  if (f != s.fan) { s.fan = f; changed = true; }
  bool cont = (fan_word & 0x0080) != 0;
  if (cont != s.continuous_fan) { s.continuous_fan = cont; changed = true; }

  uint16_t sp = readReg(data, 12, PAGE1_START);
  if (sp != s.main_setpoint_raw) { s.main_setpoint_raw = sp; changed = true; }

  uint16_t cool = readReg(data, 55, PAGE1_START);
  if (cool != s.cool_main_setpoint_raw) { s.cool_main_setpoint_raw = cool; changed = true; }

  uint16_t heat = readReg(data, 56, PAGE1_START);
  if (heat != s.heat_main_setpoint_raw) { s.heat_main_setpoint_raw = heat; changed = true; }

  uint16_t mask_word = readReg(data, 85, PAGE1_START);
  uint8_t mask = (uint8_t)(mask_word & 0xFF);
  if (mask != s.zone_enable_mask) { s.zone_enable_mask = mask; changed = true; }
  for (size_t i = 0; i < NUM_ZONES; i++) {
    bool en = (mask & (1u << i)) != 0;
    if (en != s.zones[i].enabled) { s.zones[i].enabled = en; changed = true; }
  }

  for (size_t i = 0; i < NUM_ZONES; i++) {
    uint16_t t = readReg(data, (uint16_t)(94 + i), PAGE1_START);
    if (t != s.zones[i].current_temp_raw) {
      s.zones[i].current_temp_raw = t;
      changed = true;
    }
  }

  s.page1_seen = true;
  return changed;
}

bool decodePage2(ControllerState& s, const uint8_t* data) {
  bool changed = !s.page2_seen;
  for (size_t i = 0; i < NUM_ZONES; i++) {
    uint16_t cool = readReg(data, (uint16_t)(127 + i), PAGE2_START);
    if (cool != s.zones[i].cool_setpoint_raw) {
      s.zones[i].cool_setpoint_raw = cool;
      changed = true;
    }
    uint16_t heat = readReg(data, (uint16_t)(135 + i), PAGE2_START);
    if (heat != s.zones[i].heat_setpoint_raw) {
      s.zones[i].heat_setpoint_raw = heat;
      changed = true;
    }
  }
  s.page2_seen = true;
  return changed;
}

}  // namespace

bool feedNeoFrame(ControllerState& out, const uint8_t* frame, size_t len) {
  if (frame == nullptr || len < 5) return false;
  if (frame[0] != ADDR_NEO) return false;
  if (frame[1] != FUNC_READ_HOLDING) return false;

  uint8_t bytecount = frame[2];
  size_t expected_len = (size_t)3 + (size_t)bytecount + 2;
  if (len != expected_len) return false;

  uint16_t want_crc = (uint16_t)frame[len - 2] | ((uint16_t)frame[len - 1] << 8);
  uint16_t got_crc = bridge::crc16(frame, len - 2);
  if (got_crc != want_crc) return false;

  const uint8_t* data = frame + 3;
  if (bytecount == BYTECOUNT_PAGE1) return decodePage1(out, data);
  if (bytecount == BYTECOUNT_PAGE2) return decodePage2(out, data);
  return false;
}

uint16_t activeMasterSetpoint(const ControllerState& s) {
  switch (s.mode) {
    case Mode::Cool: return s.cool_main_setpoint_raw;
    case Mode::Heat: return s.heat_main_setpoint_raw;
    case Mode::Auto: return s.cool_main_setpoint_raw;
    default: return SENTINEL_NONE;
  }
}

uint16_t activeZoneSetpoint(const ControllerState& s, size_t zone_idx) {
  if (zone_idx >= NUM_ZONES) return SENTINEL_NONE;
  switch (s.mode) {
    case Mode::Cool: return s.zones[zone_idx].cool_setpoint_raw;
    case Mode::Heat: return s.zones[zone_idx].heat_setpoint_raw;
    case Mode::Auto: return s.zones[zone_idx].cool_setpoint_raw;
    default: return SENTINEL_NONE;
  }
}

const char* modeName(Mode m) {
  switch (m) {
    case Mode::Off: return "off";
    case Mode::Cool: return "cool";
    case Mode::Heat: return "heat";
    case Mode::Fan: return "fan";
    case Mode::Auto: return "auto";
    default: return "unknown";
  }
}

const char* fanName(FanSpeed f) {
  switch (f) {
    case FanSpeed::Off: return "off";
    case FanSpeed::Low: return "low";
    case FanSpeed::Med: return "med";
    case FanSpeed::High: return "high";
    case FanSpeed::Auto: return "auto";
    default: return "unknown";
  }
}

Mode parseMode(const char* s) {
  if (s == nullptr) return Mode::Unknown;
  if (strcmp(s, "off") == 0) return Mode::Off;
  if (strcmp(s, "cool") == 0) return Mode::Cool;
  if (strcmp(s, "heat") == 0) return Mode::Heat;
  if (strcmp(s, "fan") == 0) return Mode::Fan;
  if (strcmp(s, "auto") == 0) return Mode::Auto;
  return Mode::Unknown;
}

FanSpeed parseFan(const char* s) {
  if (s == nullptr) return FanSpeed::Unknown;
  if (strcmp(s, "off") == 0) return FanSpeed::Off;
  if (strcmp(s, "low") == 0) return FanSpeed::Low;
  if (strcmp(s, "med") == 0) return FanSpeed::Med;
  if (strcmp(s, "high") == 0) return FanSpeed::High;
  if (strcmp(s, "auto") == 0) return FanSpeed::Auto;
  return FanSpeed::Unknown;
}

}  // namespace state
