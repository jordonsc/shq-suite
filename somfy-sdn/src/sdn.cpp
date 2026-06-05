#include "sdn.h"

#include <cstdio>
#include <cstring>

namespace sdn {

uint16_t checksum(const uint8_t* bus_bytes, size_t n) {
  uint16_t sum = 0;
  for (size_t i = 0; i < n; i++) sum += bus_bytes[i];
  return sum;
}

bool addrEqual(const uint8_t a[3], const uint8_t b[3]) {
  return a[0] == b[0] && a[1] == b[1] && a[2] == b[2];
}

bool addrIsBroadcast(const uint8_t a[3]) { return addrEqual(a, BROADCAST_ADDR); }

size_t buildFrame(uint8_t* buf, uint8_t msg_id, const uint8_t src[3], const uint8_t dst[3],
                  const uint8_t* data, size_t data_len) {
  const size_t total_len = HEADER_LEN + data_len + CKSUM_LEN;
  if (total_len > MAX_FRAME_LEN) return 0;

  const bool is_broadcast = addrIsBroadcast(dst);

  // Raw frame.
  uint8_t raw[MAX_FRAME_LEN];
  raw[OFF_MSG_ID] = msg_id;

  // Length field: bits0-6 = total frame length, bit7 = directed flag.
  uint8_t length_byte = (uint8_t)(total_len & 0x7F);
  if (!is_broadcast) length_byte |= DIRECTED_FLAG;
  raw[OFF_LENGTH] = length_byte;

  raw[OFF_NETWORK] = is_broadcast ? NET_BROADCAST : NET_TOOL_TO_MOTOR;

  memcpy(&raw[OFF_SRC_ADDR], src, 3);
  memcpy(&raw[OFF_DST_ADDR], dst, 3);
  if (data_len > 0 && data != nullptr) memcpy(&raw[OFF_DATA], data, data_len);

  // Invert all bytes before the checksum for the bus frame.
  const size_t payload_len = HEADER_LEN + data_len;  // bytes before checksum
  for (size_t i = 0; i < payload_len; i++) buf[i] = (uint8_t)~raw[i];

  // Checksum = sum of inverted (bus) bytes, appended UN-inverted, big-endian.
  uint16_t cksum = checksum(buf, payload_len);
  buf[payload_len] = (uint8_t)(cksum >> 8);
  buf[payload_len + 1] = (uint8_t)(cksum & 0xFF);

  return total_len;
}

size_t buildToolFrame(uint8_t* buf, uint8_t msg_id, const uint8_t dst[3],
                      const uint8_t* data, size_t data_len) {
  return buildFrame(buf, msg_id, TOOL_SRC_ADDR, dst, data, data_len);
}

bool parseFrame(const uint8_t* bus_buf, size_t len, ParsedFrame* out) {
  out->valid = false;
  if (bus_buf == nullptr || len < HEADER_LEN + CKSUM_LEN || len > MAX_FRAME_LEN) return false;

  // Checksum trailer is un-inverted on the wire; the rest is inverted.
  const uint16_t rx_cksum = ((uint16_t)bus_buf[len - 2] << 8) | bus_buf[len - 1];
  const uint16_t calc_cksum = checksum(bus_buf, len - 2);
  if (rx_cksum != calc_cksum) return false;

  // Un-invert into a local raw buffer (don't mutate the caller's buffer).
  uint8_t raw[MAX_FRAME_LEN];
  for (size_t i = 0; i < len - 2; i++) raw[i] = (uint8_t)~bus_buf[i];

  // Sanity-check the embedded length against the captured length.
  const size_t frame_len = (size_t)(raw[OFF_LENGTH] & 0x7F);
  if (frame_len != len) return false;

  out->msg_id = raw[OFF_MSG_ID];
  out->network = raw[OFF_NETWORK];
  memcpy(out->src_addr, &raw[OFF_SRC_ADDR], 3);
  memcpy(out->dst_addr, &raw[OFF_DST_ADDR], 3);

  const size_t data_len = len - HEADER_LEN - CKSUM_LEN;
  memcpy(out->data, &raw[OFF_DATA], data_len);
  out->data_len = data_len;
  out->valid = true;
  return true;
}

// ---- command payload builders --------------------------------------------

size_t payloadMoveTo(uint8_t* data, uint8_t fn, uint8_t value) {
  data[0] = fn;
  data[1] = value;  // pct / IP index; 0 for limit moves
  data[2] = 0x00;
  data[3] = 0x00;
  return 4;
}

size_t payloadMove(uint8_t* data, uint8_t direction, uint8_t duration, uint8_t speed) {
  data[0] = direction;
  data[1] = duration;
  data[2] = speed;
  return 3;
}

size_t payloadMoveOf(uint8_t* data, uint8_t fn, uint16_t param) {
  data[0] = fn;
  data[1] = (uint8_t)(param & 0xFF);  // LE
  data[2] = (uint8_t)(param >> 8);
  data[3] = 0x00;
  return 4;
}

size_t payloadStop(uint8_t* data) {
  data[0] = 0x01;  // proven STOP payload
  return 1;
}

size_t payloadSetLimit(uint8_t* data, uint8_t fn, uint8_t direction, uint16_t param) {
  data[0] = fn;
  data[1] = direction;
  data[2] = (uint8_t)(param & 0xFF);  // LE
  data[3] = (uint8_t)(param >> 8);
  return 4;
}

size_t payloadSetDirection(uint8_t* data, uint8_t dir) {
  data[0] = dir;
  return 1;
}

size_t payloadFactoryDefault(uint8_t* data, uint8_t scope) {
  data[0] = scope;
  return 1;
}

// ---- response parsers ----------------------------------------------------

PositionReport parsePosition(const uint8_t* data, size_t len) {
  PositionReport r;
  if (data == nullptr || len < 3) return r;
  r.pulses = (uint16_t)data[0] | ((uint16_t)data[1] << 8);  // LE
  r.percent = data[2];
  if (len > 3) r.tilt = data[3];
  if (len > 4) r.ip = data[4];

  if (r.pulses == POSITION_UNKNOWN || r.percent > 100) {
    r.fault = true;
    r.valid = false;
    return r;
  }
  r.valid = true;
  return r;
}

LimitsReport parseLimits(const uint8_t* data, size_t len) {
  LimitsReport r;
  if (data == nullptr || len < 4) return r;
  // Field-proven app_sdn layout (see header note + SPEC §13.4 dispute).
  r.up_pulses = (uint16_t)data[0] | ((uint16_t)data[1] << 8);
  r.down_pulses = (uint16_t)data[2] | ((uint16_t)data[3] << 8);
  r.valid = true;
  return r;
}

// ---- address helpers -----------------------------------------------------

static int hexNibble(char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  return -1;
}

bool parseAddress(const char* display, uint8_t out[3]) {
  if (display == nullptr) return false;
  // Expect "HH:HH:HH" (display order); store byte-reversed.
  uint8_t parts[3];
  size_t p = 0;
  const char* s = display;
  while (p < 3) {
    int hi = hexNibble(s[0]);
    int lo = hexNibble(s[1]);
    if (hi < 0 || lo < 0) return false;
    parts[p] = (uint8_t)((hi << 4) | lo);
    s += 2;
    p++;
    if (p < 3) {
      if (*s != ':') return false;
      s++;
    }
  }
  if (*s != '\0') return false;
  // Display "AA:BB:CC" -> raw {CC,BB,AA}.
  out[0] = parts[2];
  out[1] = parts[1];
  out[2] = parts[0];
  return true;
}

void formatAddress(const uint8_t addr[3], char* out) {
  // raw {CC,BB,AA} -> display "AA:BB:CC".
  snprintf(out, 9, "%02X:%02X:%02X", addr[2], addr[1], addr[0]);
}

}  // namespace sdn
