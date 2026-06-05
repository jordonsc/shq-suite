// Bounded ring buffer of wire/protocol diagnostic events (SPEC §6.4). Pure C++, no Arduino
// deps, so it builds + unit-tests on the host. Browsable over HTTP (/errors) and summarised
// into per-class counters for /stats. The caller (bus.cpp) supplies the timestamp so this
// stays Arduino-free.

#pragma once

#include <cstddef>
#include <cstdint>

namespace errlog {

enum class Class : uint8_t {
  CHECKSUM = 0,  // RX checksum mismatch
  FRAMING,       // bad length / truncated / parity error
  TIMEOUT,       // no/short reply within window
  NACK,          // motor returned 0x6F
  STALL,         // position unchanged during movement (fault)
  POS_UNKNOWN,   // pulses==0xFFFF / pct>100
  OFFLINE,       // comms-loss transition
  ONLINE,        // comms-recovered transition
  COLLISION,     // unexpected/garbled bus activity while transmitting
  COUNT_
};

constexpr size_t NUM_CLASSES = (size_t)Class::COUNT_;
constexpr size_t RING_SIZE = 128;
constexpr size_t MSG_MAX = 48;
constexpr size_t HEX_MAX = 24;  // truncated raw bytes captured per entry

const char* className(Class c);

struct Entry {
  uint32_t seq = 0;        // monotonic, 1-based; 0 = empty slot
  uint32_t t_ms = 0;       // ms since boot (caller-supplied)
  Class cls = Class::CHECKSUM;
  bool has_addr = false;
  uint8_t addr[3] = {0, 0, 0};  // raw node addr (if applicable)
  char msg[MSG_MAX] = {0};
  uint8_t hex[HEX_MAX] = {0};
  uint8_t hex_len = 0;
};

class ErrorLog {
 public:
  // Record an event. `addr` may be null (no associated device). `raw`/`raw_len` are the
  // offending bytes (truncated to HEX_MAX). Bumps the per-class counter and the ring.
  void record(uint32_t t_ms, Class cls, const uint8_t* addr, const char* msg,
              const uint8_t* raw, size_t raw_len);

  // Convenience: record with no raw bytes.
  void record(uint32_t t_ms, Class cls, const uint8_t* addr, const char* msg) {
    record(t_ms, cls, addr, msg, nullptr, 0);
  }

  uint32_t counter(Class c) const { return counters_[(size_t)c]; }
  uint32_t total() const { return seq_; }

  // Copy up to `max` newest-first entries into `out`. Returns the number copied.
  size_t newest(Entry* out, size_t max) const;

  void clear();

 private:
  Entry ring_[RING_SIZE];
  uint32_t seq_ = 0;
  uint32_t counters_[NUM_CLASSES] = {0};
};

}  // namespace errlog
