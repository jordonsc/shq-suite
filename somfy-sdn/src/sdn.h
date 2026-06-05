// Somfy Digital Network (SDN) RS485 protocol — pure C++, no Arduino deps.
//
// Framing/checksum/inversion ported VERBATIM (semantics) from the field-proven
// matter-apps/common/features/app_sdn.cpp (sdn_build_frame / sdn_parse_frame). The UART I/O
// that wrapped them there lives in bus.cpp here; this file is just the wire encoding plus the
// command-payload builders and response parsers, so it can be host-unit-tested under
// [env:native] (see SPEC.md §6.1). Marked "CONFIRM" where a calibration payload still needs a
// Somfy Set Pro capture (SPEC §13) — these build the best-known guess and are exercised on the
// bench via the HTTP /send endpoint.
//
// Wire rules (SPEC §5.2), all field-proven:
//   Raw frame:  [MSG_ID][LEN|flags][NET][SRC x3][DST x3][DATA..][CKSUM x2]
//   Bus frame:  every byte EXCEPT the 2-byte checksum is bitwise-inverted (~b).
//   Checksum:   16-bit sum of the inverted (bus) bytes preceding it, big-endian, un-inverted.
//   Addresses:  3 bytes, byte-reversed from display form (01:00:23 -> {0x23,0x00,0x01}).
//   Data:       multi-byte values little-endian. Checksum is the only big-endian field.

#pragma once

#include <cstddef>
#include <cstdint>

namespace sdn {

// ---- line parameters (SPEC §5.1) -----------------------------------------
constexpr uint32_t BAUD_RATE = 4800;          // 8-O-1
constexpr uint32_t RESPONSE_TIMEOUT_MS = 1000;  // conservative first-byte timeout (as today)
constexpr uint32_t INTER_BYTE_TIMEOUT_MS = 50;
constexpr uint32_t REPLY_BODY_TIMEOUT_MS = 200;
constexpr uint8_t RETRY_COUNT = 3;
constexpr uint32_t RETRY_DELAY_MS = 200;
constexpr size_t MAX_FRAME_LEN = 32;

// ---- frame layout offsets (raw frame) ------------------------------------
constexpr size_t OFF_MSG_ID = 0;
constexpr size_t OFF_LENGTH = 1;   // bits0-6 = total len incl cksum, bit7 = directed flag
constexpr size_t OFF_NETWORK = 2;
constexpr size_t OFF_SRC_ADDR = 3; // 3 bytes
constexpr size_t OFF_DST_ADDR = 6; // 3 bytes
constexpr size_t OFF_DATA = 9;
constexpr size_t HEADER_LEN = 9;   // header before data
constexpr size_t CKSUM_LEN = 2;
constexpr uint8_t DIRECTED_FLAG = 0x80;

// ---- network bytes (raw) -------------------------------------------------
constexpr uint8_t NET_TOOL_TO_MOTOR = 0xF9;  // directed tool -> motor
constexpr uint8_t NET_BROADCAST = 0xF0;      // broadcast
constexpr uint8_t NET_MOTOR_TO_TOOL = 0x9F;  // motor -> tool response

// Our tool source address: display 01:00:00 -> raw {0x00,0x00,0x01} (byte-reversed), as in the
// proven code. Broadcast destination = {0xFF,0xFF,0xFF}.
constexpr uint8_t TOOL_SRC_ADDR[3] = {0x00, 0x00, 0x01};
constexpr uint8_t BROADCAST_ADDR[3] = {0xFF, 0xFF, 0xFF};

// ---- message IDs (raw, pre-inversion) — SPEC §5.3 ------------------------
enum MsgId : uint8_t {
  MSG_CTRL_MOVE            = 0x01,  // forced move up/down (we prefer MOVETO)
  MSG_CTRL_STOP            = 0x02,
  MSG_CTRL_MOVETO          = 0x03,
  MSG_CTRL_MOVEOF          = 0x04,  // relative move (pulses / 10 ms) — move-by-steps
  MSG_CTRL_WINK            = 0x05,  // identify ("which blind is this") — CONFIRM payload
  MSG_GET_MOTOR_POSITION   = 0x0C,
  MSG_POST_MOTOR_POSITION  = 0x0D,
  MSG_GET_MOTOR_STATUS     = 0x0E,
  MSG_POST_MOTOR_STATUS    = 0x0F,
  MSG_SET_MOTOR_LIMITS     = 0x11,  // set top/bottom limit — CONFIRM payload
  MSG_SET_MOTOR_DIRECTION  = 0x12,  // rotation direction — CONFIRM payload
  MSG_SET_MOTOR_ROLLING_SPEED = 0x13,  // deferred
  MSG_SET_FACTORY_DEFAULT  = 0x1F,  // reset positions — CONFIRM payload (capture from Set Pro)
  MSG_GET_MOTOR_LIMITS     = 0x21,
  MSG_GET_MOTOR_DIRECTION  = 0x22,
  MSG_GET_FACTORY_DEFAULT  = 0x2F,
  MSG_POST_MOTOR_LIMITS    = 0x31,
  MSG_POST_MOTOR_DIRECTION = 0x32,
  MSG_POST_FACTORY_DEFAULT = 0x3F,
  MSG_GET_NODE_ADDR        = 0x40,
  MSG_GET_NODE_LABEL       = 0x45,
  MSG_GET_NODE_SERIAL      = 0x4C,
  MSG_SET_NODE_DISCOVERY   = 0x50,
  MSG_SET_NODE_LABEL       = 0x55,
  MSG_POST_NODE_ADDR       = 0x60,
  MSG_POST_NODE_LABEL      = 0x65,
  MSG_NACK                 = 0x6F,  // command rejected
  MSG_POST_NODE_SERIAL     = 0x6C,
  MSG_ACK                  = 0x7F,  // command accepted
};

// ---- CTRL_MOVETO (0x03) function bytes — SPEC §5.4 -----------------------
constexpr uint8_t MOVETO_DOWN_LIMIT = 0x00;  // close (down limit)
constexpr uint8_t MOVETO_UP_LIMIT   = 0x01;  // open (up limit)
constexpr uint8_t MOVETO_IP         = 0x02;  // to intermediate-position index
constexpr uint8_t MOVETO_PERCENT    = 0x04;  // to percentage (0-100, Somfy native)

// ---- CTRL_MOVE (0x01) momentary / commissioning jog ----------------------
// Payload {direction, duration, speed} (ccutrer/somfy_sdn reference). This is the deadman/
// momentary move used to position a motor BEFORE limits are set (relative/absolute moves NACK
// with limits_not_set/wrong_position until then). duration 0 = run continuously until CANCEL;
// 0x0A..0xFF = a timed nudge (Set Pro style) that auto-stops.
constexpr uint8_t MOVE_DIR_DOWN   = 0x00;
constexpr uint8_t MOVE_DIR_UP     = 0x01;
constexpr uint8_t MOVE_DIR_CANCEL = 0x02;
constexpr uint8_t MOVE_SPEED_NORMAL = 0x00;
constexpr uint8_t MOVE_SPEED_SLOW   = 0x02;
constexpr uint8_t MOVE_DURATION_MIN = 0x0A;

// ---- CTRL_MOVEOF (0x04) function bytes -----------------------------------
constexpr uint8_t MOVEOF_NEXT_IP_DOWN = 0x00;
constexpr uint8_t MOVEOF_NEXT_IP_UP   = 0x01;
constexpr uint8_t MOVEOF_PULSES_DOWN  = 0x02;
constexpr uint8_t MOVEOF_PULSES_UP    = 0x03;
constexpr uint8_t MOVEOF_MS_DOWN      = 0x04;  // N x 10 ms
constexpr uint8_t MOVEOF_MS_UP        = 0x05;

// ---- SET_MOTOR_LIMITS (0x11) — SPEC §5.4 (CONFIRM) -----------------------
constexpr uint8_t LIMIT_FN_AT_CURRENT = 0x01;  // set limit at current position (common op)
constexpr uint8_t LIMIT_FN_ABSOLUTE   = 0x02;  // set at absolute pulse count (param)
constexpr uint8_t LIMIT_DIR_DOWN = 0x00;       // CONFIRM via Set Pro
constexpr uint8_t LIMIT_DIR_UP   = 0x01;

// ---- SET_MOTOR_DIRECTION (0x12) — (CONFIRM) ------------------------------
constexpr uint8_t DIRECTION_STANDARD = 0x00;
constexpr uint8_t DIRECTION_REVERSED = 0x01;

// ---- SET_FACTORY_DEFAULT (0x1F) reset scopes — 1-byte payload ------------
// From ccutrer/somfy_sdn. "Reset positions" should use RESET_LIMITS (re-commission limits)
// rather than RESET_ALL, which also wipes the node address/label and would orphan the motor.
constexpr uint8_t RESET_ALL           = 0x00;
constexpr uint8_t RESET_GROUPS        = 0x01;
constexpr uint8_t RESET_LIMITS        = 0x11;
constexpr uint8_t RESET_ROTATION      = 0x12;
constexpr uint8_t RESET_ROLLING_SPEED = 0x13;
constexpr uint8_t RESET_IPS           = 0x15;
constexpr uint8_t RESET_LOCKS         = 0x17;

// Position sentinel: motor reports unknown/fault.
constexpr uint16_t POSITION_UNKNOWN = 0xFFFF;

enum class MovementState : uint8_t { IDLE = 0, MOVING_UP = 1, MOVING_DOWN = 2 };

// ---- parsed frame --------------------------------------------------------
struct ParsedFrame {
  bool valid = false;
  uint8_t msg_id = 0;
  uint8_t network = 0;
  uint8_t src_addr[3] = {0, 0, 0};
  uint8_t dst_addr[3] = {0, 0, 0};
  uint8_t data[MAX_FRAME_LEN] = {0};
  size_t data_len = 0;
};

// ---- framing -------------------------------------------------------------

// 16-bit sum of bytes (the SDN checksum is computed over the INVERTED bus bytes).
uint16_t checksum(const uint8_t* bus_bytes, size_t n);

// Build a bus-ready (inverted + checksummed) frame into `buf` (>= MAX_FRAME_LEN).
// `src`/`dst` are raw (byte-reversed) addresses. A dst of BROADCAST_ADDR sets the broadcast
// network byte and clears the directed flag. Returns total wire length incl. checksum, or 0
// if it wouldn't fit. Ported from sdn_build_frame.
size_t buildFrame(uint8_t* buf, uint8_t msg_id, const uint8_t src[3], const uint8_t dst[3],
                  const uint8_t* data, size_t data_len);

// Convenience: build with the standard tool source address (TOOL_SRC_ADDR).
size_t buildToolFrame(uint8_t* buf, uint8_t msg_id, const uint8_t dst[3],
                      const uint8_t* data, size_t data_len);

// Parse a received bus frame (inverted, with un-inverted checksum trailer). Validates the
// checksum and length, un-inverts, and fills `out`. Does NOT mutate `bus_buf`. Returns
// out->valid. Ported from sdn_parse_frame (made non-mutating for testability).
bool parseFrame(const uint8_t* bus_buf, size_t len, ParsedFrame* out);

// ---- command payload builders --------------------------------------------
// Each writes the DATA field and returns its length. Pair with buildToolFrame(MSG_*, ...).

// CTRL_MOVETO to a named target. For MOVETO_PERCENT / MOVETO_IP, `value` is the pct/IP index.
size_t payloadMoveTo(uint8_t* data, uint8_t fn, uint8_t value);
// CTRL_MOVE momentary move {direction, duration, speed}. duration 0 = continuous; else a timed
// nudge. Works before limits are set (unlike MoveTo/MoveOf).
size_t payloadMove(uint8_t* data, uint8_t direction, uint8_t duration, uint8_t speed);
// CTRL_MOVEOF relative move. `param` is pulses or x10ms depending on fn.
size_t payloadMoveOf(uint8_t* data, uint8_t fn, uint16_t param);
// CTRL_STOP (the proven payload is {0x01}).
size_t payloadStop(uint8_t* data);
// SET_MOTOR_LIMITS {fn, direction, param_lo, param_hi}. (CONFIRM)
size_t payloadSetLimit(uint8_t* data, uint8_t fn, uint8_t direction, uint16_t param);
// SET_MOTOR_DIRECTION {dir}. (CONFIRM)
size_t payloadSetDirection(uint8_t* data, uint8_t dir);
// SET_FACTORY_DEFAULT — 1-byte reset scope (RESET_* above). Confirmed via ccutrer reference +
// hardware (the earlier empty-payload version was a no-op).
size_t payloadFactoryDefault(uint8_t* data, uint8_t scope);

// ---- response parsers ----------------------------------------------------

struct PositionReport {
  bool valid = false;       // a POST_MOTOR_POSITION with a usable reading
  bool fault = false;       // pulses==0xFFFF or pct>100 => unknown / fault
  uint16_t pulses = 0;      // LE pulse position (data[0..1])
  uint8_t percent = 0;      // native Somfy % (data[2]); 0=open .. 100=closed — USE THIS
  uint8_t tilt = 0;         // data[3] tilt %
  uint8_t ip = 0xFF;        // data[4] IP index (0xFF = none)
};
// Parse a POST_MOTOR_POSITION (0x0D) data block. Uses data[2] directly as percent (the
// position-reporting fix, commit 6477359 — do NOT recompute from pulses). SPEC §5.4.
PositionReport parsePosition(const uint8_t* data, size_t len);

struct LimitsReport {
  bool valid = false;
  uint16_t up_pulses = 0;
  uint16_t down_pulses = 0;
};
// Parse a POST_MOTOR_LIMITS (0x31) data block. NOTE: byte offsets are disputed (SPEC §5.4 /
// §13.4) — public docs say data[2..3]=limit with data[0..1] reserved, but the field-proven
// app_sdn reads up=[0..1] down=[2..3] from a single query. We port the proven behaviour and
// flag it; resolve on the wire with the sniffer.
LimitsReport parseLimits(const uint8_t* data, size_t len);

// ---- address helpers -----------------------------------------------------

// Parse a display address "01:00:23" into raw byte-reversed form {0x23,0x00,0x01}.
// Returns false on malformed input.
bool parseAddress(const char* display, uint8_t out[3]);
// Format a raw address {0x23,0x00,0x01} as display "01:00:23". `out` must be >= 9 bytes.
void formatAddress(const uint8_t addr[3], char* out);
bool addrEqual(const uint8_t a[3], const uint8_t b[3]);
bool addrIsBroadcast(const uint8_t a[3]);

// ---- HA <-> Somfy position inversion (SPEC §5.5) -------------------------
// Somfy: pct 0 = up/open, 100 = down/closed. HA cover: 0 = closed, 100 = open. Defined here
// once; firmware keeps native Somfy percent everywhere and only converts at the WS/HA boundary.
inline uint8_t haToSomfy(uint8_t ha_position) {
  return (ha_position > 100) ? 0 : (uint8_t)(100 - ha_position);
}
inline uint8_t somfyToHa(uint8_t somfy_pct) {
  return (somfy_pct > 100) ? 0 : (uint8_t)(100 - somfy_pct);
}

}  // namespace sdn
