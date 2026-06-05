#include "errlog.h"

#include <cstring>

namespace errlog {

const char* className(Class c) {
  switch (c) {
    case Class::CHECKSUM: return "CHECKSUM";
    case Class::FRAMING: return "FRAMING";
    case Class::TIMEOUT: return "TIMEOUT";
    case Class::NACK: return "NACK";
    case Class::STALL: return "STALL";
    case Class::POS_UNKNOWN: return "POS_UNKNOWN";
    case Class::OFFLINE: return "OFFLINE";
    case Class::ONLINE: return "ONLINE";
    case Class::COLLISION: return "COLLISION";
    default: return "?";
  }
}

void ErrorLog::record(uint32_t t_ms, Class cls, const uint8_t* addr, const char* msg,
                      const uint8_t* raw, size_t raw_len) {
  if ((size_t)cls < NUM_CLASSES) counters_[(size_t)cls]++;

  uint32_t s = ++seq_;
  Entry& e = ring_[(s - 1) % RING_SIZE];
  e.seq = s;
  e.t_ms = t_ms;
  e.cls = cls;
  e.has_addr = (addr != nullptr);
  if (addr != nullptr) memcpy(e.addr, addr, 3);
  else memset(e.addr, 0, 3);

  if (msg != nullptr) {
    strncpy(e.msg, msg, MSG_MAX - 1);
    e.msg[MSG_MAX - 1] = '\0';
  } else {
    e.msg[0] = '\0';
  }

  size_t n = (raw_len > HEX_MAX) ? HEX_MAX : raw_len;
  if (raw != nullptr && n > 0) memcpy(e.hex, raw, n);
  e.hex_len = (uint8_t)((raw != nullptr) ? n : 0);
}

size_t ErrorLog::newest(Entry* out, size_t max) const {
  size_t n = 0;
  // Walk back from the highest seq.
  for (uint32_t s = seq_; s >= 1 && n < max; s--) {
    const Entry& e = ring_[(s - 1) % RING_SIZE];
    if (e.seq != s) break;  // older than what the ring still holds
    out[n++] = e;
    if (s == 1) break;  // avoid uint32 underflow
  }
  return n;
}

void ErrorLog::clear() {
  seq_ = 0;
  for (size_t i = 0; i < RING_SIZE; i++) ring_[i] = Entry{};
  for (size_t i = 0; i < NUM_CLASSES; i++) counters_[i] = 0;
}

}  // namespace errlog
