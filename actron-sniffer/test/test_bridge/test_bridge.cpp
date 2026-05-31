// Host-side unit tests for the MITM streaming bridge. Run with: pio test -e native
//
// Validates: passthrough modes leave bytes alone; INJECT rewrites only the matching register
// bytes; the recomputed CRC is valid for the OUTPUT frame; unmodified frames keep their
// original CRC; multiple rules in one frame all apply; out-of-range / non-matching rules
// have no effect; non-func-03 frames (master broadcasts etc.) pass through unchanged;
// onGap correctly resets state for back-to-back frames.

#include <unity.h>
#include <cstdint>
#include <cstddef>
#include <cstring>
#include <vector>

#include "bridge.h"

using bridge::StreamingBridge;
using bridge::Rule;
using bridge::crc16;

// ---- helpers --------------------------------------------------------------

static std::vector<uint8_t> runFrame(StreamingBridge& b, const std::vector<uint8_t>& in) {
  std::vector<uint8_t> out;
  out.reserve(in.size());
  b.onGap();
  for (uint8_t byte : in) out.push_back(b.processByte(byte));
  return out;
}

// Build a complete CRC-valid func-03 response. payload_idx_to_byte computes each data byte
// from its 0-based position within the payload (so callers can encode whatever register
// pattern they like for the input frame).
template <typename F>
static std::vector<uint8_t> makeResponse(uint8_t addr, uint8_t bytecount, F payload_idx_to_byte) {
  std::vector<uint8_t> f;
  f.reserve(3 + bytecount + 2);
  f.push_back(addr);
  f.push_back(0x03);
  f.push_back(bytecount);
  for (size_t i = 0; i < bytecount; i++) f.push_back(payload_idx_to_byte(i));
  uint16_t c = crc16(f.data(), f.size());
  f.push_back((uint8_t)(c & 0xFF));
  f.push_back((uint8_t)(c >> 8));
  return f;
}

// Set the bytes for a given Modbus register inside a response frame buffer.
static void setReg(std::vector<uint8_t>& frame, uint16_t start_addr, uint16_t reg, uint16_t value) {
  size_t off = 3 + (size_t)(reg - start_addr) * 2;
  frame[off]     = (uint8_t)(value >> 8);
  frame[off + 1] = (uint8_t)(value & 0xFF);
}

// Recompute and stamp a valid CRC over a frame whose data we have mutated in place.
static void restampCrc(std::vector<uint8_t>& frame) {
  size_t pre = frame.size() - 2;
  uint16_t c = crc16(frame.data(), pre);
  frame[pre]     = (uint8_t)(c & 0xFF);
  frame[pre + 1] = (uint8_t)(c >> 8);
}

// ---- tests ---------------------------------------------------------------

void test_off_passes_through_with_or_without_rules() {
  StreamingBridge b;
  b.setMode(StreamingBridge::OFF);
  Rule rules[] = {{127, 0xCAFE}};
  b.setRules(rules, 1);
  std::vector<uint8_t> in = {0x66, 0x03, 0xF8, 0xAA, 0xBB, 0xCC, 0xDD};
  auto out = runFrame(b, in);
  TEST_ASSERT_EQUAL_size_t(in.size(), out.size());
  TEST_ASSERT_EQUAL_MEMORY(in.data(), out.data(), in.size());
  TEST_ASSERT_FALSE(b.modified());
}

void test_passthru_is_identity() {
  StreamingBridge b;
  b.setMode(StreamingBridge::PASSTHRU);
  Rule rules[] = {{127, 0xCAFE}};
  b.setRules(rules, 1);
  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)(i ^ 0x55); });
  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());
  TEST_ASSERT_EQUAL_MEMORY(frame.data(), out.data(), frame.size());
  TEST_ASSERT_FALSE(b.modified());
}

void test_inject_unrecognised_frame_passes_through() {
  // Master broadcast: 00 10 ... — not a slave func-03 response, must pass through unchanged.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule rules[] = {{127, 0xCAFE}};
  b.setRules(rules, 1);
  std::vector<uint8_t> frame = {0x00, 0x10, 0x00, 0x04, 0x00, 0x06, 0x0C,
                                0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
                                0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC,
                                0xDE, 0xAD};
  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());
  TEST_ASSERT_EQUAL_MEMORY(frame.data(), out.data(), frame.size());
  TEST_ASSERT_FALSE(b.modified());
}

void test_inject_no_matching_rule_in_range_passes_through() {
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  // page-1 response covers regs 2..125; rule for 200 falls outside the range.
  Rule rules[] = {{200, 0xCAFE}};
  b.setRules(rules, 1);
  auto frame = makeResponse(0x66, 0xF8, [](size_t i) { return (uint8_t)(i * 7); });
  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());
  TEST_ASSERT_EQUAL_MEMORY(frame.data(), out.data(), frame.size());
  TEST_ASSERT_FALSE(b.modified());
}

void test_inject_rewrites_page1_register_and_recomputes_crc() {
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  // Page-1 starts at reg 2. Substitute reg 12 (main setpoint) with 220 (22.0°C).
  Rule rules[] = {{12, 220}};
  b.setRules(rules, 1);
  auto frame = makeResponse(0x66, 0xF8, [](size_t i) { return (uint8_t)i; });
  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());

  // Reg 12 high byte at data_idx (12-2)*2 = 20 -> out[3+20]=out[23], low byte at out[24].
  TEST_ASSERT_EQUAL_UINT8(0x00, out[23]);          // high byte of 220
  TEST_ASSERT_EQUAL_UINT8(0xDC, out[24]);          // low byte (220 = 0xDC)
  // Surrounding bytes untouched.
  TEST_ASSERT_EQUAL_UINT8(frame[22], out[22]);
  TEST_ASSERT_EQUAL_UINT8(frame[25], out[25]);
  // CRC bytes must validate against the output frame, not the input.
  uint16_t outcrc = crc16(out.data(), out.size() - 2);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc & 0xFF), out[out.size() - 2]);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc >> 8),  out[out.size() - 1]);
  TEST_ASSERT_TRUE(b.modified());
}

void test_inject_rewrites_page2_zone_setpoint_and_recomputes_crc() {
  // The actual reason for the MITM bridge: zone setpoint in page-2 (the 0x67-pulse path can't
  // do this). Reg 127 = first per-zone cool setpoint. Set it to 230 (23.0°C).
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule rules[] = {{127, 230}};
  b.setRules(rules, 1);
  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)(i ^ 0xAA); });

  // Sanity: confirm that BEFORE injection, reg 127 in the input frame is NOT already 230 — so
  // our test exercises the genuine "value changes" path, not the no-op short-circuit.
  size_t off = 3 + (127 - 126) * 2;
  uint16_t before = ((uint16_t)frame[off] << 8) | frame[off + 1];
  TEST_ASSERT_NOT_EQUAL_UINT16(230, before);

  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());
  TEST_ASSERT_EQUAL_UINT8(0x00, out[off]);
  TEST_ASSERT_EQUAL_UINT8(0xE6, out[off + 1]);   // 230 = 0xE6
  uint16_t outcrc = crc16(out.data(), out.size() - 2);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc & 0xFF), out[out.size() - 2]);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc >> 8),  out[out.size() - 1]);
  TEST_ASSERT_TRUE(b.modified());
}

void test_inject_value_already_matches_leaves_frame_byte_identical() {
  // If a rule's value matches what's already on the wire, modified_ stays false and we forward
  // the original CRC unchanged. This guards against unnecessary churn / spurious "modified"
  // counters when the rule is essentially a no-op.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  auto frame = makeResponse(0x66, 0xF8, [](size_t i) { return (uint8_t)(i * 3); });
  // Read the value currently stored at reg 50 in the freshly built frame and target THAT value.
  size_t off = 3 + (50 - 2) * 2;
  uint16_t target = ((uint16_t)frame[off] << 8) | frame[off + 1];
  Rule rules[] = {{50, target}};
  b.setRules(rules, 1);

  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());
  TEST_ASSERT_EQUAL_MEMORY(frame.data(), out.data(), frame.size());
  TEST_ASSERT_FALSE(b.modified());
}

void test_inject_multiple_rules_in_one_frame() {
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  // Three substitutions in one page-1 response.
  Rule rules[] = {{12, 235}, {55, 235}, {100, 0x0123}};
  b.setRules(rules, 3);
  auto frame = makeResponse(0x66, 0xF8, [](size_t i) { return (uint8_t)(i + 1); });
  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());

  auto check = [&](uint16_t reg, uint16_t v) {
    size_t off = 3 + (reg - 2) * 2;
    TEST_ASSERT_EQUAL_UINT8((uint8_t)(v >> 8), out[off]);
    TEST_ASSERT_EQUAL_UINT8((uint8_t)(v & 0xFF), out[off + 1]);
  };
  check(12, 235);
  check(55, 235);
  check(100, 0x0123);
  uint16_t outcrc = crc16(out.data(), out.size() - 2);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc & 0xFF), out[out.size() - 2]);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc >> 8),  out[out.size() - 1]);
  TEST_ASSERT_TRUE(b.modified());
}

void test_output_matches_independent_offline_rewrite() {
  // Cross-check: build the SAME modification offline (mutate the bytes, recompute CRC) and
  // confirm the streaming bridge produces a byte-identical result. This protects against
  // running-CRC bugs where the streamed CRC subtly diverges from a bulk re-CRC.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule rules[] = {{8, 0x1234}, {123, 0xABCD}};
  b.setRules(rules, 2);

  auto frame = makeResponse(0x66, 0xF8, [](size_t i) { return (uint8_t)(i * 13 + 7); });

  // Reference: in-place mutation + CRC restamp.
  auto ref = frame;
  setReg(ref, /*start=*/2, 8,   0x1234);
  setReg(ref, /*start=*/2, 123, 0xABCD);
  restampCrc(ref);

  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(ref.size(), out.size());
  TEST_ASSERT_EQUAL_MEMORY(ref.data(), out.data(), ref.size());
}

void test_two_back_to_back_frames_each_handled_independently() {
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule rules[] = {{127, 230}};
  b.setRules(rules, 1);

  auto p2 = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)(i + 1); });
  auto p1 = makeResponse(0x66, 0xF8, [](size_t i) { return (uint8_t)(i + 1); });

  // First frame: page-2, rule matches.
  auto out2 = runFrame(b, p2);
  TEST_ASSERT_TRUE(b.modified());
  size_t off = 3 + (127 - 126) * 2;
  TEST_ASSERT_EQUAL_UINT8(0x00, out2[off]);
  TEST_ASSERT_EQUAL_UINT8(0xE6, out2[off + 1]);
  uint16_t crc2 = crc16(out2.data(), out2.size() - 2);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(crc2 & 0xFF), out2[out2.size() - 2]);

  // Second frame: page-1, same rule's reg (127) is out of range -> passthrough.
  auto out1 = runFrame(b, p1);
  TEST_ASSERT_FALSE(b.modified());
  TEST_ASSERT_EQUAL_MEMORY(p1.data(), out1.data(), p1.size());
}

void test_gap_mid_frame_resets_state() {
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule rules[] = {{127, 230}};
  b.setRules(rules, 1);
  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)i; });

  // Feed half the frame, then an idle-gap reset, then a fresh whole frame. The truncated
  // half should not corrupt the subsequent good frame.
  b.onGap();
  for (size_t i = 0; i < frame.size() / 2; i++) (void)b.processByte(frame[i]);
  b.onGap();
  std::vector<uint8_t> out;
  for (uint8_t byte : frame) out.push_back(b.processByte(byte));

  size_t off = 3 + (127 - 126) * 2;
  TEST_ASSERT_EQUAL_UINT8(0x00, out[off]);
  TEST_ASSERT_EQUAL_UINT8(0xE6, out[off + 1]);
  uint16_t outcrc = crc16(out.data(), out.size() - 2);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc & 0xFF), out[out.size() - 2]);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc >> 8),  out[out.size() - 1]);
}

void test_rule_at_first_and_last_register_of_each_page() {
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  // Page-1 first reg = 2; last = 125. Page-2 first = 126; last = 247.
  Rule rules[] = {{2, 0xAA55}, {125, 0x5AA5}, {126, 0xBB44}, {247, 0x4422}};
  b.setRules(rules, 4);

  auto p1 = makeResponse(0x66, 0xF8, [](size_t i) { return (uint8_t)i; });
  auto p2 = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)(i + 1); });

  auto outp1 = runFrame(b, p1);
  TEST_ASSERT_EQUAL_UINT8(0xAA, outp1[3]);                       // reg 2 high
  TEST_ASSERT_EQUAL_UINT8(0x55, outp1[4]);                       // reg 2 low
  TEST_ASSERT_EQUAL_UINT8(0x5A, outp1[3 + (125-2)*2]);           // reg 125 high
  TEST_ASSERT_EQUAL_UINT8(0xA5, outp1[3 + (125-2)*2 + 1]);       // reg 125 low

  auto outp2 = runFrame(b, p2);
  TEST_ASSERT_EQUAL_UINT8(0xBB, outp2[3]);                       // reg 126 high
  TEST_ASSERT_EQUAL_UINT8(0x44, outp2[4]);                       // reg 126 low
  TEST_ASSERT_EQUAL_UINT8(0x44, outp2[3 + (247-126)*2]);         // reg 247 high
  TEST_ASSERT_EQUAL_UINT8(0x22, outp2[3 + (247-126)*2 + 1]);     // reg 247 low
}

void test_set_or_add_rule_upserts_and_appends() {
  // setOrAddRule should overwrite an existing rule's value (not duplicate it), and append a
  // new rule when the register isn't present. Used by the per-frame scramble test.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule rules[] = {{12, 0x1111}, {135, 0x2222}};
  b.setRules(rules, 2);
  TEST_ASSERT_EQUAL_size_t(2, b.ruleCount());

  // Upsert existing reg 135 — count stays 2.
  b.setOrAddRule(135, 0x3333);
  TEST_ASSERT_EQUAL_size_t(2, b.ruleCount());

  // Add new reg 247 (the LAST valid register in a page-2 response — bytecount 0xF4 = 244,
  // covering regs 126..247 inclusive). Count becomes 3.
  b.setOrAddRule(247, 0xDEAD);
  TEST_ASSERT_EQUAL_size_t(3, b.ruleCount());

  // Verify the upserts took effect: drive a page-2 frame and check the rewritten bytes.
  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)i; });
  auto out = runFrame(b, frame);
  // reg 135 in page-2: data idx (135-126)*2 = 18, frame bytes 21-22.
  TEST_ASSERT_EQUAL_UINT8(0x33, out[21]);
  TEST_ASSERT_EQUAL_UINT8(0x33, out[22]);
  // reg 247 in page-2: data idx (247-126)*2 = 242, frame bytes 245-246.
  TEST_ASSERT_EQUAL_UINT8(0xDE, out[245]);
  TEST_ASSERT_EQUAL_UINT8(0xAD, out[246]);
  // CRC valid for output.
  uint16_t outcrc = crc16(out.data(), out.size() - 2);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc & 0xFF), out[out.size() - 2]);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc >> 8),  out[out.size() - 1]);
}

void test_pulse_applies_for_configured_frames_then_expires() {
  // Arm a pulse rule for 2 frames; verify it fires on the next 2 recognised frames and then
  // stops. The third frame should pass through unmodified (assuming no persistent rule).
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule pulse[] = {{243, 0x0028}};   // the page-2 commit-signal candidate from the live capture
  b.setPulse(pulse, 1, 2);
  TEST_ASSERT_EQUAL_size_t(2, b.pulseRemaining());

  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)(i + 1); });
  // reg 243 in page-2: data idx (243-126)*2 = 234, frame bytes 237-238.
  size_t off = 3 + (243 - 126) * 2;

  // Frame 1: pulse fires (3 remaining starts as 2, decrements to 1 after this frame).
  auto out1 = runFrame(b, frame);
  TEST_ASSERT_EQUAL_UINT8(0x00, out1[off]);
  TEST_ASSERT_EQUAL_UINT8(0x28, out1[off + 1]);
  TEST_ASSERT_EQUAL_size_t(1, b.pulseRemaining());

  // Frame 2: still fires (1 -> 0 after).
  auto out2 = runFrame(b, frame);
  TEST_ASSERT_EQUAL_UINT8(0x00, out2[off]);
  TEST_ASSERT_EQUAL_UINT8(0x28, out2[off + 1]);
  TEST_ASSERT_EQUAL_size_t(0, b.pulseRemaining());

  // Frame 3: pulse has expired, byte is unchanged.
  auto out3 = runFrame(b, frame);
  TEST_ASSERT_EQUAL_UINT8(frame[off], out3[off]);
  TEST_ASSERT_EQUAL_UINT8(frame[off + 1], out3[off + 1]);
  TEST_ASSERT_FALSE(b.modified());
}

void test_pulse_takes_precedence_over_persistent_rule() {
  // A persistent rule for reg 135 = 220 + a pulse rule for reg 135 = 230 for 1 frame.
  // Frame 1: pulse wins, byte 21-22 = 0x00 0xE6. Frame 2: persistent wins, byte 21-22 = 0x00 0xDC.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule persistent[] = {{135, 220}};
  b.setRules(persistent, 1);
  Rule pulse[] = {{135, 230}};
  b.setPulse(pulse, 1, 1);

  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)i; });
  auto out1 = runFrame(b, frame);
  TEST_ASSERT_EQUAL_UINT8(0x00, out1[21]);
  TEST_ASSERT_EQUAL_UINT8(0xE6, out1[22]);  // pulse value: 230

  auto out2 = runFrame(b, frame);
  TEST_ASSERT_EQUAL_UINT8(0x00, out2[21]);
  TEST_ASSERT_EQUAL_UINT8(0xDC, out2[22]);  // persistent value: 220
}

void test_pulse_does_not_decrement_on_unrecognised_frames() {
  // Master broadcast (00 10 ...) shouldn't burn a pulse. Arm pulse for 1 frame, feed a
  // master broadcast, then feed a real page-2 response — the response should still see the
  // pulse rule applied.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  Rule pulse[] = {{135, 230}};
  b.setPulse(pulse, 1, 1);

  std::vector<uint8_t> bcast = {0x00, 0x10, 0x00, 0x04, 0x00, 0x06, 0x0C,
                                 0x11, 0x22, 0x33, 0x44, 0x55, 0x66,
                                 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC,
                                 0xDE, 0xAD};
  auto bout = runFrame(b, bcast);
  TEST_ASSERT_EQUAL_size_t(1, b.pulseRemaining());  // pulse not consumed by broadcast

  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)i; });
  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_UINT8(0xE6, out[22]);  // pulse fired on the real response
  TEST_ASSERT_EQUAL_size_t(0, b.pulseRemaining());
}

void test_replay_substitutes_entire_frame_from_template_including_crc() {
  // Build a CRC-valid page-2 response with one register set, treat it as the "captured
  // template", then run a DIFFERENT page-2 frame through the bridge with replay enabled —
  // expect the bridge's output to byte-for-byte equal the template (including the original
  // CRC). No CRC recomputation should happen.
  auto orig = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)i; });
  // Template = bytes [3..frame.size()-1] = 244 data + 2 CRC = 246 bytes.
  std::vector<uint8_t> tmpl(orig.begin() + 3, orig.end());
  TEST_ASSERT_EQUAL_size_t(246, tmpl.size());

  // A different "live" frame that the NEO is sending — different data.
  auto live = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)(i ^ 0xAA); });

  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  b.setReplayPage2Data(tmpl.data());
  b.setReplayEnabled(true);

  auto out = runFrame(b, live);

  // The first 3 bytes (header `66 03 F4`) are identical in both frames; everything after
  // should equal the template, including the trailing 2 CRC bytes that were captured.
  TEST_ASSERT_EQUAL_UINT8(0x66, out[0]);
  TEST_ASSERT_EQUAL_UINT8(0x03, out[1]);
  TEST_ASSERT_EQUAL_UINT8(0xF4, out[2]);
  for (size_t i = 0; i < tmpl.size(); i++) {
    TEST_ASSERT_EQUAL_UINT8(tmpl[i], out[3 + i]);
  }
  // And the output's CRC bytes must equal the ORIGINAL template's CRC bytes, not a recomputed
  // CRC over the live frame.
  TEST_ASSERT_EQUAL_UINT8(orig[orig.size() - 2], out[out.size() - 2]);
  TEST_ASSERT_EQUAL_UINT8(orig[orig.size() - 1], out[out.size() - 1]);
}

void test_replay_off_means_rules_apply_as_usual() {
  // Sanity: with replay loaded but disabled, rule-based substitution should still work.
  std::vector<uint8_t> dummy(246, 0xAB);
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  b.setReplayPage2Data(dummy.data());
  b.setReplayEnabled(false);
  Rule rules[] = {{135, 230}};
  b.setRules(rules, 1);

  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)i; });
  auto out = runFrame(b, frame);
  // reg 135 should be 230 (rule), CRC recomputed over the modified frame, NOT equal to the
  // dummy template byte.
  TEST_ASSERT_EQUAL_UINT8(0x00, out[21]);
  TEST_ASSERT_EQUAL_UINT8(0xE6, out[22]);
  uint16_t outcrc = crc16(out.data(), out.size() - 2);
  TEST_ASSERT_EQUAL_UINT8((uint8_t)(outcrc & 0xFF), out[out.size() - 2]);
}

void test_replay_only_fires_when_matching_template_loaded() {
  // Replay enabled but no page-2 template loaded: bridge should fall through to rule-based
  // behaviour (= passthrough since no rules) and NOT zero out the bytes from an unloaded
  // template.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  b.setReplayEnabled(true);
  // Only load page-1, then feed a page-2 frame — it should NOT be replaced.
  std::vector<uint8_t> p1tmpl(250, 0xCC);
  b.setReplayPage1Data(p1tmpl.data());

  auto frame = makeResponse(0x66, 0xF4, [](size_t i) { return (uint8_t)i; });
  auto out = runFrame(b, frame);
  TEST_ASSERT_EQUAL_size_t(frame.size(), out.size());
  TEST_ASSERT_EQUAL_MEMORY(frame.data(), out.data(), frame.size());
}

void test_isMidFrame_reflects_recognised_in_progress_frames() {
  // isMidFrame() should be false when no frame is in progress (byte_idx_=0 or after frame end),
  // true between byte 1 and byte frame_len_-1 of a recognised frame, false for unrecognised
  // frames (where frame_len_ stays 0). Drives the firmware's "skip external reset mid-frame"
  // policy.
  StreamingBridge b;
  b.setMode(StreamingBridge::INJECT);
  TEST_ASSERT_FALSE(b.isMidFrame());

  // Push a page-2 header (recognised). After byte 2 (bytecount), frame_len_ is set.
  (void)b.processByte(0x66);
  TEST_ASSERT_FALSE(b.isMidFrame());      // byte_idx_=1, frame_len_ still 0
  (void)b.processByte(0x03);
  TEST_ASSERT_FALSE(b.isMidFrame());      // byte_idx_=2, frame_len_ still 0
  (void)b.processByte(0xF4);
  TEST_ASSERT_TRUE(b.isMidFrame());       // byte_idx_=3, frame_len_=249 — now mid-frame

  // Process most of the data; should still be mid-frame until byte frame_len_.
  for (int i = 0; i < 240; i++) (void)b.processByte((uint8_t)i);
  TEST_ASSERT_TRUE(b.isMidFrame());

  // Push enough bytes to complete the frame (need to reach byte_idx_ == 249 = 246 more bytes
  // total = 6 more after the 243 already in). Build a valid CRC-prefix frame so we don't
  // accidentally land in a weird state.
  // We've already fed 243 bytes; need 6 more (4 data + 2 CRC) to reach 249.
  std::vector<uint8_t> frame(249);
  frame[0] = 0x66; frame[1] = 0x03; frame[2] = 0xF4;
  for (size_t i = 0; i < 244; i++) frame[3 + i] = (uint8_t)i;
  uint16_t c = crc16(frame.data(), frame.size() - 2);
  frame[247] = c & 0xFF; frame[248] = c >> 8;
  // Reset and run the whole frame cleanly.
  b.onGap();
  for (uint8_t byt : frame) (void)b.processByte(byt);
  // Frame completed -> bridge resets internally -> not mid-frame.
  TEST_ASSERT_FALSE(b.isMidFrame());

  // Now an unrecognised master broadcast — frame_len_ stays 0, so never mid-frame even though
  // bytes are flowing through.
  std::vector<uint8_t> bcast = {0x00, 0x10, 0x00, 0x04, 0x00, 0x06, 0x0C,
                                 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88};
  for (uint8_t byt : bcast) {
    (void)b.processByte(byt);
    TEST_ASSERT_FALSE(b.isMidFrame());
  }
}

void test_crc16_matches_known_modbus_test_vector() {
  // Canonical Modbus check: CRC of "0x02 0x07" is 0x4112 (low,high). Guards the CRC routine
  // itself against accidental polynomial/init regressions.
  uint8_t v[] = {0x02, 0x07};
  uint16_t c = crc16(v, sizeof(v));
  TEST_ASSERT_EQUAL_UINT16(0x1241, c);
}

void setUp() {}
void tearDown() {}

int main(int /*argc*/, char** /*argv*/) {
  UNITY_BEGIN();
  RUN_TEST(test_off_passes_through_with_or_without_rules);
  RUN_TEST(test_passthru_is_identity);
  RUN_TEST(test_inject_unrecognised_frame_passes_through);
  RUN_TEST(test_inject_no_matching_rule_in_range_passes_through);
  RUN_TEST(test_inject_rewrites_page1_register_and_recomputes_crc);
  RUN_TEST(test_inject_rewrites_page2_zone_setpoint_and_recomputes_crc);
  RUN_TEST(test_inject_value_already_matches_leaves_frame_byte_identical);
  RUN_TEST(test_inject_multiple_rules_in_one_frame);
  RUN_TEST(test_output_matches_independent_offline_rewrite);
  RUN_TEST(test_two_back_to_back_frames_each_handled_independently);
  RUN_TEST(test_gap_mid_frame_resets_state);
  RUN_TEST(test_rule_at_first_and_last_register_of_each_page);
  RUN_TEST(test_set_or_add_rule_upserts_and_appends);
  RUN_TEST(test_pulse_applies_for_configured_frames_then_expires);
  RUN_TEST(test_pulse_takes_precedence_over_persistent_rule);
  RUN_TEST(test_pulse_does_not_decrement_on_unrecognised_frames);
  RUN_TEST(test_replay_substitutes_entire_frame_from_template_including_crc);
  RUN_TEST(test_replay_off_means_rules_apply_as_usual);
  RUN_TEST(test_replay_only_fires_when_matching_template_loaded);
  RUN_TEST(test_isMidFrame_reflects_recognised_in_progress_frames);
  RUN_TEST(test_crc16_matches_known_modbus_test_vector);
  return UNITY_END();
}
