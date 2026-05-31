// MITM bridge logic for the Actron NEO ↔ indoor-unit RS485 bus.
//
// Pure C++ (no Arduino deps) so it can be unit-tested on the host. The class is a streaming
// byte rewriter: fed one byte at a time, it returns one byte to forward to the other side of
// the bus. On Modbus func-03 slave responses (0x66/0x67/0x68 read holding regs) it can
// substitute specific register values inline and emit a recomputed CRC at the frame trailer
// so the indoor master sees a valid Modbus frame. All other frames pass through unmodified.

#pragma once
#include <cstddef>
#include <cstdint>

namespace bridge {

// Standard Modbus RTU CRC-16 (poly 0xA001, init 0xFFFF). Appended to the wire little-endian.
uint16_t crc16(const uint8_t* data, size_t n);

struct Rule {
  uint16_t reg;     // absolute Modbus register number (e.g. 127 = first per-zone cool setpoint)
  uint16_t value;   // 16-bit value to substitute (big-endian on the wire, like the rest)
};

class StreamingBridge {
 public:
  // RESPOND: the firmware acts as 0x66 from the board's POV — it drops all forwarding
  // (the NEO is fully isolated by the cut bus + our blocked TX) and emits a stored template
  // back to the board when it polls for page-1 or page-2. Driven by main.cpp; the bridge
  // object itself treats RESPOND like OFF (no per-byte work).
  enum Mode { OFF, PASSTHRU, INJECT, RESPOND };
  static constexpr size_t MAX_RULES = 16;

  StreamingBridge();

  void setMode(Mode m);
  Mode mode() const { return mode_; }

  // Replace the rule set. n > MAX_RULES is clamped.
  void setRules(const Rule* rules, size_t n);
  size_t ruleCount() const { return n_rules_; }

  // Upsert a single rule. If a rule for `reg` exists, update its value; otherwise append
  // (subject to MAX_RULES). Lets the caller update one rule each frame without clobbering
  // the rest of the rule set — used by the per-frame reg-248 "scramble" test for the zone-
  // setpoint commit hypothesis.
  void setOrAddRule(uint16_t reg, uint16_t value);

  // Pure replay: substitute *every* byte after the 3-byte header (which is identical to all
  // 0x66 func-03 responses anyway) from a saved template, including the trailing CRC. No
  // CRC computation — what was captured is what gets forwarded byte-for-byte.
  //   Page-1 template = 250 bytes: 248 data (regs 2..125) + 2 CRC trailer.
  //   Page-2 template = 246 bytes: 244 data (regs 126..247) + 2 CRC trailer.
  static constexpr size_t kReplayPage1Bytes = 250;
  static constexpr size_t kReplayPage2Bytes = 246;
  void setReplayPage1Data(const uint8_t* data);  // copies kReplayPage1Bytes
  void setReplayPage2Data(const uint8_t* data);  // copies kReplayPage2Bytes
  void setReplayEnabled(bool on);
  void clearReplay();
  bool replayEnabled() const { return replay_enabled_; }
  bool replayPage1Loaded() const { return replay_p1_loaded_; }
  bool replayPage2Loaded() const { return replay_p2_loaded_; }

  // Arm a transient set of substitution rules that apply only for the next `n_frames`
  // recognised frames (slave func-03 responses we know the layout of), then auto-expire.
  // Pulse rules take precedence over persistent rules for the duration. Mirrors the 0x67
  // emulator's pulsen mechanism — used to test whether zone-setpoint writes commit when
  // a "high-byte-to-zero" pulse on reg 243 (page-2) / reg 65, 67 (page-1) accompanies the
  // value change.
  void setPulse(const Rule* rules, size_t n, size_t n_frames);
  size_t pulseRemaining() const { return pulse_remaining_; }
  size_t pulseRuleCount() const { return n_pulse_rules_; }

  // Reset per-frame state. Call when the bus has been idle for > t3.5 (frame boundary).
  void onGap();

  // Feed one received byte. Returns the byte to forward to the other side of the bus.
  // For OFF / PASSTHRU, output == input. For INJECT, frames matching a known func-03
  // response layout may have data bytes substituted and a recomputed CRC emitted in place
  // of the original trailer.
  uint8_t processByte(uint8_t in);

  // True while the bridge is partway through a recognised slave func-03 response (we've
  // seen the bytecount but not yet reached the end). External callers (e.g. the firmware's
  // gap detector) can use this to avoid resetting bridge state mid-frame on a false-positive
  // idle — bytes have stopped arriving from the bridge's POV not because the wire is idle
  // but because main-loop work (HTTP, TX-FIFO backpressure) deferred our reads.
  bool isMidFrame() const {
    return frame_len_ != 0 && byte_idx_ > 0 && byte_idx_ < frame_len_;
  }

  // Did the most recently completed frame actually have any byte rewritten?
  // Stays valid across the inter-frame idle gap (so a caller can poll after pumping a frame
  // through). A mid-frame onGap() abort does not change this — only a fully completed frame
  // updates it.
  bool modified() const { return last_modified_; }

 private:
  Mode mode_;
  Rule rules_[MAX_RULES];
  size_t n_rules_;
  Rule pulse_rules_[MAX_RULES];
  size_t n_pulse_rules_;
  size_t pulse_remaining_;
  uint8_t replay_p1_data_[kReplayPage1Bytes];
  uint8_t replay_p2_data_[kReplayPage2Bytes];
  bool replay_p1_loaded_;
  bool replay_p2_loaded_;
  bool replay_enabled_;

  // Per-frame state (reset by onGap and at end-of-frame).
  size_t byte_idx_;       // 0-based position within the current frame
  uint8_t addr_;          // byte 0
  uint8_t func_;          // byte 1
  uint8_t bytecount_;     // byte 2 (for func-03 responses)
  uint16_t start_addr_;   // first register in the response payload
  size_t frame_len_;      // total wire length incl. CRC; 0 = "frame layout not recognised"
  bool curr_modified_;    // in-flight: any data byte actually changed from the input?
  bool last_modified_;    // result of the most recent completed frame (observable post-frame)
  uint16_t running_crc_;  // CRC accumulated over bytes we've EMITTED (not received)

  bool findRule(uint16_t reg, uint16_t& value_out) const;
  void resetFrameState();
};

}  // namespace bridge
