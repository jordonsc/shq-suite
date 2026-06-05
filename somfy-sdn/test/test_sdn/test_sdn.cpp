// Host-side unit tests for the pure-C++ SDN protocol core. Run with: pio test -e native
//
// Validates: bus-frame inversion + big-endian checksum against a hand-computed vector;
// build/parse roundtrip for directed + broadcast frames; checksum rejection; length-sanity
// rejection; command payload builders; position + limits response parsing (incl. fault
// sentinel); address parse/format roundtrip; and the HA<->Somfy position inversion.

#include <unity.h>

#include <cstdint>
#include <cstring>

#include "sdn.h"

using namespace sdn;

void setUp() {}
void tearDown() {}

// ---- checksum / inversion against a hand-computed vector -------------------
// CTRL_STOP (0x02) broadcast, data {0x01}. Worked out by hand in the test comments:
//   raw    = 02 0C F0 00 00 01 FF FF FF 01
//   invert = FD F3 0F FF FF FE 00 00 00 FE   (sum = 0x05F9)
//   wire   = FD F3 0F FF FF FE 00 00 00 FE 05 F9   (checksum big-endian, un-inverted)
void test_build_matches_hand_computed_bus_frame() {
  uint8_t buf[MAX_FRAME_LEN];
  uint8_t data[1] = {0x01};
  size_t n = buildToolFrame(buf, MSG_CTRL_STOP, BROADCAST_ADDR, data, 1);
  TEST_ASSERT_EQUAL_UINT(12, n);
  const uint8_t expect[12] = {0xFD, 0xF3, 0x0F, 0xFF, 0xFF, 0xFE,
                              0x00, 0x00, 0x00, 0xFE, 0x05, 0xF9};
  TEST_ASSERT_EQUAL_HEX8_ARRAY(expect, buf, 12);
}

// ---- directed frame sets the length directed flag + tool->motor network ----
void test_directed_frame_flags_and_roundtrip() {
  uint8_t dst[3];
  TEST_ASSERT_TRUE(parseAddress("01:00:23", dst));  // -> raw {0x23,0x00,0x01}
  TEST_ASSERT_EQUAL_HEX8(0x23, dst[0]);
  TEST_ASSERT_EQUAL_HEX8(0x00, dst[1]);
  TEST_ASSERT_EQUAL_HEX8(0x01, dst[2]);

  uint8_t buf[MAX_FRAME_LEN];
  uint8_t data[4];
  size_t dn = payloadMoveTo(data, MOVETO_PERCENT, 40);
  size_t n = buildToolFrame(buf, MSG_CTRL_MOVETO, dst, data, dn);
  TEST_ASSERT_EQUAL_UINT(HEADER_LEN + 4 + CKSUM_LEN, n);

  // Length byte (un-inverted) must carry the directed flag for a non-broadcast dst.
  uint8_t raw_len_byte = (uint8_t)~buf[OFF_LENGTH];
  TEST_ASSERT_TRUE((raw_len_byte & DIRECTED_FLAG) != 0);
  TEST_ASSERT_EQUAL_UINT(n, raw_len_byte & 0x7F);
  TEST_ASSERT_EQUAL_HEX8(NET_TOOL_TO_MOTOR, (uint8_t)~buf[OFF_NETWORK]);

  ParsedFrame pf;
  TEST_ASSERT_TRUE(parseFrame(buf, n, &pf));
  TEST_ASSERT_TRUE(pf.valid);
  TEST_ASSERT_EQUAL_HEX8(MSG_CTRL_MOVETO, pf.msg_id);
  TEST_ASSERT_EQUAL_HEX8(NET_TOOL_TO_MOTOR, pf.network);
  TEST_ASSERT_EQUAL_HEX8_ARRAY(dst, pf.dst_addr, 3);
  TEST_ASSERT_EQUAL_HEX8_ARRAY(TOOL_SRC_ADDR, pf.src_addr, 3);
  TEST_ASSERT_EQUAL_UINT(4, pf.data_len);
  TEST_ASSERT_EQUAL_HEX8(MOVETO_PERCENT, pf.data[0]);
  TEST_ASSERT_EQUAL_HEX8(40, pf.data[1]);
}

void test_broadcast_frame_clears_directed_flag() {
  uint8_t buf[MAX_FRAME_LEN];
  size_t n = buildToolFrame(buf, MSG_GET_NODE_ADDR, BROADCAST_ADDR, nullptr, 0);
  uint8_t raw_len_byte = (uint8_t)~buf[OFF_LENGTH];
  TEST_ASSERT_TRUE((raw_len_byte & DIRECTED_FLAG) == 0);
  TEST_ASSERT_EQUAL_HEX8(NET_BROADCAST, (uint8_t)~buf[OFF_NETWORK]);

  ParsedFrame pf;
  TEST_ASSERT_TRUE(parseFrame(buf, n, &pf));
  TEST_ASSERT_EQUAL_HEX8(NET_BROADCAST, pf.network);
  TEST_ASSERT_EQUAL_UINT(0, pf.data_len);
}

void test_parse_rejects_bad_checksum() {
  uint8_t buf[MAX_FRAME_LEN];
  uint8_t data[1] = {0x01};
  size_t n = buildToolFrame(buf, MSG_CTRL_STOP, BROADCAST_ADDR, data, 1);
  buf[n - 1] ^= 0xFF;  // corrupt the low checksum byte
  ParsedFrame pf;
  TEST_ASSERT_FALSE(parseFrame(buf, n, &pf));
  TEST_ASSERT_FALSE(pf.valid);
}

void test_parse_rejects_length_mismatch() {
  uint8_t buf[MAX_FRAME_LEN];
  uint8_t data[1] = {0x01};
  size_t n = buildToolFrame(buf, MSG_CTRL_STOP, BROADCAST_ADDR, data, 1);
  // Truncate by one byte: checksum recomputed over a shorter span won't match, and the
  // embedded length won't equal len either. Must be rejected, not parsed.
  ParsedFrame pf;
  TEST_ASSERT_FALSE(parseFrame(buf, n - 1, &pf));
}

void test_parse_rejects_too_short() {
  uint8_t buf[4] = {0, 0, 0, 0};
  ParsedFrame pf;
  TEST_ASSERT_FALSE(parseFrame(buf, 4, &pf));
}

// ---- payload builders -----------------------------------------------------
void test_payload_builders() {
  uint8_t d[4];

  TEST_ASSERT_EQUAL_UINT(4, payloadMoveTo(d, MOVETO_UP_LIMIT, 0));
  TEST_ASSERT_EQUAL_HEX8(0x01, d[0]);
  TEST_ASSERT_EQUAL_HEX8(0x00, d[1]);

  TEST_ASSERT_EQUAL_UINT(4, payloadMoveTo(d, MOVETO_DOWN_LIMIT, 0));
  TEST_ASSERT_EQUAL_HEX8(0x00, d[0]);

  // CTRL_MOVE timed nudge up -> {0x01, dur, speed}
  TEST_ASSERT_EQUAL_UINT(3, payloadMove(d, MOVE_DIR_UP, 20, MOVE_SPEED_NORMAL));
  TEST_ASSERT_EQUAL_HEX8(MOVE_DIR_UP, d[0]);
  TEST_ASSERT_EQUAL_HEX8(20, d[1]);
  TEST_ASSERT_EQUAL_HEX8(MOVE_SPEED_NORMAL, d[2]);

  // MOVEOF 600 pulses up -> {0x03, 0x58, 0x02, 0x00} (600 = 0x0258, LE)
  TEST_ASSERT_EQUAL_UINT(4, payloadMoveOf(d, MOVEOF_PULSES_UP, 600));
  TEST_ASSERT_EQUAL_HEX8(MOVEOF_PULSES_UP, d[0]);
  TEST_ASSERT_EQUAL_HEX8(0x58, d[1]);
  TEST_ASSERT_EQUAL_HEX8(0x02, d[2]);

  TEST_ASSERT_EQUAL_UINT(1, payloadStop(d));
  TEST_ASSERT_EQUAL_HEX8(0x01, d[0]);

  // SET_MOTOR_LIMITS at current position, up direction.
  TEST_ASSERT_EQUAL_UINT(4, payloadSetLimit(d, LIMIT_FN_AT_CURRENT, LIMIT_DIR_UP, 0));
  TEST_ASSERT_EQUAL_HEX8(LIMIT_FN_AT_CURRENT, d[0]);
  TEST_ASSERT_EQUAL_HEX8(LIMIT_DIR_UP, d[1]);

  TEST_ASSERT_EQUAL_UINT(1, payloadSetDirection(d, DIRECTION_REVERSED));
  TEST_ASSERT_EQUAL_HEX8(0x01, d[0]);

  // Factory default: 1-byte reset scope (RESET_LIMITS for "reset positions").
  TEST_ASSERT_EQUAL_UINT(1, payloadFactoryDefault(d, RESET_LIMITS));
  TEST_ASSERT_EQUAL_HEX8(0x11, d[0]);
}

// ---- response parsers -----------------------------------------------------
void test_parse_position_valid_uses_data2_percent() {
  // pulses = 0x0258 (600) LE, percent = 40, tilt 0, ip 0xFF
  uint8_t data[5] = {0x58, 0x02, 40, 0x00, 0xFF};
  PositionReport r = parsePosition(data, 5);
  TEST_ASSERT_TRUE(r.valid);
  TEST_ASSERT_FALSE(r.fault);
  TEST_ASSERT_EQUAL_UINT16(600, r.pulses);
  TEST_ASSERT_EQUAL_UINT8(40, r.percent);
  TEST_ASSERT_EQUAL_HEX8(0xFF, r.ip);
}

void test_parse_position_fault_on_sentinels() {
  uint8_t bad_pulses[3] = {0xFF, 0xFF, 50};
  PositionReport r1 = parsePosition(bad_pulses, 3);
  TEST_ASSERT_FALSE(r1.valid);
  TEST_ASSERT_TRUE(r1.fault);

  uint8_t bad_pct[3] = {0x10, 0x00, 200};  // pct>100
  PositionReport r2 = parsePosition(bad_pct, 3);
  TEST_ASSERT_FALSE(r2.valid);
  TEST_ASSERT_TRUE(r2.fault);
}

void test_parse_limits() {
  uint8_t data[4] = {0x10, 0x00, 0x84, 0x03};  // up=0x0010(16), down=0x0384(900)
  LimitsReport r = parseLimits(data, 4);
  TEST_ASSERT_TRUE(r.valid);
  TEST_ASSERT_EQUAL_UINT16(16, r.up_pulses);
  TEST_ASSERT_EQUAL_UINT16(900, r.down_pulses);
}

// ---- address helpers ------------------------------------------------------
void test_address_roundtrip() {
  uint8_t raw[3];
  TEST_ASSERT_TRUE(parseAddress("AB:CD:EF", raw));
  TEST_ASSERT_EQUAL_HEX8(0xEF, raw[0]);
  TEST_ASSERT_EQUAL_HEX8(0xCD, raw[1]);
  TEST_ASSERT_EQUAL_HEX8(0xAB, raw[2]);
  char disp[9];
  formatAddress(raw, disp);
  TEST_ASSERT_EQUAL_STRING("AB:CD:EF", disp);

  uint8_t junk[3];
  TEST_ASSERT_FALSE(parseAddress("01:00", junk));
  TEST_ASSERT_FALSE(parseAddress("0G:00:00", junk));
  TEST_ASSERT_FALSE(parseAddress("01:00:00:00", junk));
}

// ---- HA <-> Somfy inversion ----------------------------------------------
void test_ha_somfy_inversion() {
  TEST_ASSERT_EQUAL_UINT8(0, haToSomfy(100));    // HA open  -> Somfy 0 (up limit)
  TEST_ASSERT_EQUAL_UINT8(100, haToSomfy(0));    // HA closed-> Somfy 100 (down limit)
  TEST_ASSERT_EQUAL_UINT8(60, haToSomfy(40));
  TEST_ASSERT_EQUAL_UINT8(40, somfyToHa(60));
  // Round trips
  for (int p = 0; p <= 100; p++) {
    TEST_ASSERT_EQUAL_UINT8((uint8_t)p, somfyToHa(haToSomfy((uint8_t)p)));
  }
}

int main(int /*argc*/, char** /*argv*/) {
  UNITY_BEGIN();
  RUN_TEST(test_build_matches_hand_computed_bus_frame);
  RUN_TEST(test_directed_frame_flags_and_roundtrip);
  RUN_TEST(test_broadcast_frame_clears_directed_flag);
  RUN_TEST(test_parse_rejects_bad_checksum);
  RUN_TEST(test_parse_rejects_length_mismatch);
  RUN_TEST(test_parse_rejects_too_short);
  RUN_TEST(test_payload_builders);
  RUN_TEST(test_parse_position_valid_uses_data2_percent);
  RUN_TEST(test_parse_position_fault_on_sentinels);
  RUN_TEST(test_parse_limits);
  RUN_TEST(test_address_roundtrip);
  RUN_TEST(test_ha_somfy_inversion);
  return UNITY_END();
}
