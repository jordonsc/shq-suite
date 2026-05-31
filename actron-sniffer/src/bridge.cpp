#include "bridge.h"

namespace bridge {

static constexpr uint16_t kCrcInit = 0xFFFF;
static constexpr uint16_t kCrcPoly = 0xA001;

uint16_t crc16(const uint8_t* data, size_t n) {
  uint16_t c = kCrcInit;
  for (size_t i = 0; i < n; i++) {
    c ^= data[i];
    for (int k = 0; k < 8; k++) c = (c & 1) ? ((c >> 1) ^ kCrcPoly) : (c >> 1);
  }
  return c;
}

static inline uint16_t crc16_step(uint16_t c, uint8_t b) {
  c ^= b;
  for (int k = 0; k < 8; k++) c = (c & 1) ? ((c >> 1) ^ kCrcPoly) : (c >> 1);
  return c;
}

// Known func-03 response shapes on the Actron bus (see FINDINGS §7):
//   bytecount 0xF8 (248) -> page 1, regs 2..125
//   bytecount 0xF4 (244) -> page 2, regs 126..247
// Returns true and fills out_start / out_total if recognised.
static bool classifyResponse(uint8_t addr, uint8_t func, uint8_t bytecount,
                             uint16_t& out_start, size_t& out_total) {
  if (func != 0x03) return false;
  if (addr != 0x66 && addr != 0x67 && addr != 0x68) return false;
  if (bytecount == 0xF8) { out_start = 2;   out_total = 3 + 248 + 2; return true; }
  if (bytecount == 0xF4) { out_start = 126; out_total = 3 + 244 + 2; return true; }
  return false;
}

StreamingBridge::StreamingBridge()
    : mode_(OFF), n_rules_(0), n_pulse_rules_(0), pulse_remaining_(0),
      replay_p1_loaded_(false), replay_p2_loaded_(false), replay_enabled_(false),
      byte_idx_(0), addr_(0), func_(0), bytecount_(0),
      start_addr_(0), frame_len_(0), curr_modified_(false), last_modified_(false),
      running_crc_(kCrcInit) {}

void StreamingBridge::setMode(Mode m) {
  mode_ = m;
  last_modified_ = false;
  resetFrameState();
}

void StreamingBridge::setRules(const Rule* rules, size_t n) {
  if (n > MAX_RULES) n = MAX_RULES;
  for (size_t i = 0; i < n; i++) rules_[i] = rules[i];
  n_rules_ = n;
}

void StreamingBridge::setOrAddRule(uint16_t reg, uint16_t value) {
  for (size_t i = 0; i < n_rules_; i++) {
    if (rules_[i].reg == reg) { rules_[i].value = value; return; }
  }
  if (n_rules_ < MAX_RULES) {
    rules_[n_rules_].reg = reg;
    rules_[n_rules_].value = value;
    n_rules_++;
  }
}

void StreamingBridge::setPulse(const Rule* rules, size_t n, size_t n_frames) {
  if (n > MAX_RULES) n = MAX_RULES;
  for (size_t i = 0; i < n; i++) pulse_rules_[i] = rules[i];
  n_pulse_rules_ = n;
  pulse_remaining_ = n_frames;
}

void StreamingBridge::setReplayPage1Data(const uint8_t* data) {
  for (size_t i = 0; i < kReplayPage1Bytes; i++) replay_p1_data_[i] = data[i];
  replay_p1_loaded_ = true;
}

void StreamingBridge::setReplayPage2Data(const uint8_t* data) {
  for (size_t i = 0; i < kReplayPage2Bytes; i++) replay_p2_data_[i] = data[i];
  replay_p2_loaded_ = true;
}

void StreamingBridge::setReplayEnabled(bool on) { replay_enabled_ = on; }

void StreamingBridge::clearReplay() {
  replay_p1_loaded_ = false;
  replay_p2_loaded_ = false;
  replay_enabled_ = false;
}

void StreamingBridge::onGap() {
  resetFrameState();
  // last_modified_ deliberately preserved — a caller polling after a complete frame should
  // still see its result even if onGap is invoked between frames.
}

void StreamingBridge::resetFrameState() {
  byte_idx_ = 0;
  addr_ = 0;
  func_ = 0;
  bytecount_ = 0;
  start_addr_ = 0;
  frame_len_ = 0;
  curr_modified_ = false;
  running_crc_ = kCrcInit;
}

bool StreamingBridge::findRule(uint16_t reg, uint16_t& value_out) const {
  // Pulse rules take precedence over persistent rules while armed. So you can set a persistent
  // value for a register and a different transient pulse value, and the pulse wins for its
  // configured duration. This mirrors the 0x67 emulator's "pulse over ovr" semantics.
  if (pulse_remaining_ > 0) {
    for (size_t i = 0; i < n_pulse_rules_; i++) {
      if (pulse_rules_[i].reg == reg) { value_out = pulse_rules_[i].value; return true; }
    }
  }
  for (size_t i = 0; i < n_rules_; i++) {
    if (rules_[i].reg == reg) { value_out = rules_[i].value; return true; }
  }
  return false;
}

uint8_t StreamingBridge::processByte(uint8_t in) {
  // OFF / PASSTHRU: zero-cost forward; no CRC bookkeeping.
  if (mode_ != INJECT) return in;

  uint8_t out = in;

  switch (byte_idx_) {
    case 0:
      addr_ = in;
      running_crc_ = crc16_step(kCrcInit, in);
      break;
    case 1:
      func_ = in;
      running_crc_ = crc16_step(running_crc_, in);
      break;
    case 2: {
      bytecount_ = in;
      running_crc_ = crc16_step(running_crc_, in);
      uint16_t start; size_t total;
      if (classifyResponse(addr_, func_, bytecount_, start, total)) {
        start_addr_ = start;
        frame_len_ = total;
      }
      break;
    }
    default: {
      if (frame_len_ == 0) break;  // unrecognised frame: pure passthrough

      // Replay mode (when enabled and a template is loaded for this frame's bytecount): byte-
      // for-byte substitution from byte_idx_ 3 onwards — covers data AND the trailing 2 CRC
      // bytes — with zero CRC recomputation. The forwarded frame is an exact copy of whatever
      // was captured into the template, so the CRC remains the original from that capture.
      const uint8_t* replay_tmpl = nullptr;
      if (replay_enabled_) {
        if (bytecount_ == 0xF8 && replay_p1_loaded_) replay_tmpl = replay_p1_data_;
        else if (bytecount_ == 0xF4 && replay_p2_loaded_) replay_tmpl = replay_p2_data_;
      }
      if (replay_tmpl) {
        if (byte_idx_ >= 3 && byte_idx_ < frame_len_) {
          out = replay_tmpl[byte_idx_ - 3];
          if (out != in) curr_modified_ = true;
        }
        break;
      }

      // Rule-based substitution (the existing path).
      const size_t crc_lo_idx = 3 + bytecount_;
      const size_t crc_hi_idx = 4 + bytecount_;

      if (byte_idx_ < crc_lo_idx) {
        // data byte: maps to (start_addr + data_idx/2) — high byte at even data_idx.
        const size_t data_idx = byte_idx_ - 3;
        const uint16_t reg = (uint16_t)(start_addr_ + data_idx / 2);
        const bool high_byte = ((data_idx & 1) == 0);
        uint16_t value;
        if (findRule(reg, value)) {
          out = high_byte ? (uint8_t)(value >> 8) : (uint8_t)(value & 0xFF);
          if (out != in) curr_modified_ = true;
        }
        running_crc_ = crc16_step(running_crc_, out);
      } else if (byte_idx_ == crc_lo_idx) {
        if (curr_modified_) out = (uint8_t)(running_crc_ & 0xFF);
      } else if (byte_idx_ == crc_hi_idx) {
        if (curr_modified_) out = (uint8_t)(running_crc_ >> 8);
      }
      break;
    }
  }

  byte_idx_++;

  // End-of-frame: publish the in-flight modified flag, decrement the pulse counter (only on
  // recognised frames — master broadcasts shouldn't burn a pulse), and reset for the next
  // frame. For unrecognised frames (frame_len_ == 0) we rely on onGap() at the bus-idle
  // boundary; last_modified_ stays at its previous value (those frames are never "modified"
  // anyway, and pulses aren't applied to them).
  if (frame_len_ != 0 && byte_idx_ >= frame_len_) {
    last_modified_ = curr_modified_;
    if (pulse_remaining_ > 0) pulse_remaining_--;
    resetFrameState();
  }

  return out;
}

}  // namespace bridge
