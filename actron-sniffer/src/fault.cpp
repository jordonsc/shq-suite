#include "fault.h"

#include <cstring>

namespace fault {

namespace {

// Indexed by Code. Order must match the enum exactly.
const char* const kSlugs[CODE_COUNT] = {
    "clock_stalled", "bus_offline", "ws_capacity", "heap_low",
};

const char* const kDescriptions[CODE_COUNT] = {
    "monotonic clock has stopped advancing",
    "no configured motor is answering the bus",
    "all WebSocket client slots are occupied",
    "free heap below the reconnect floor",
};

Registry g_registry;

}  // namespace

const char* slug(Code c) {
  const uint8_t i = (uint8_t)c;
  return (i < CODE_COUNT) ? kSlugs[i] : "ok";
}

const char* describe(Code c) {
  const uint8_t i = (uint8_t)c;
  return (i < CODE_COUNT) ? kDescriptions[i] : "";
}

void Registry::raise(Code c, const char* detail) {
  const uint8_t i = (uint8_t)c;
  if (i >= CODE_COUNT) return;
  mask_ |= (1u << i);
  if (counts_[i] != 0xFFFF) counts_[i]++;
  if (detail != nullptr) {
    strncpy(detail_[i], detail, DETAIL_MAX - 1);
    detail_[i][DETAIL_MAX - 1] = '\0';
  }
}

void Registry::clear(Code c) {
  const uint8_t i = (uint8_t)c;
  if (i >= CODE_COUNT) return;
  mask_ &= ~(1u << i);
  // The detail is deliberately left in place: a cleared fault's last detail is still the best
  // answer to "what happened earlier?", and nothing reads it while the bit is down.
}

void Registry::clearAll() {
  mask_ = 0;
}

bool Registry::active(Code c) const {
  const uint8_t i = (uint8_t)c;
  return (i < CODE_COUNT) && ((mask_ & (1u << i)) != 0);
}

Code Registry::worst() const {
  for (uint8_t i = 0; i < CODE_COUNT; i++) {
    if ((mask_ & (1u << i)) != 0) return (Code)i;
  }
  return Code::Count;
}

const char* Registry::worstSlug() const {
  const Code c = worst();
  return (c == Code::Count) ? "ok" : slug(c);
}

const char* Registry::worstDetail() const {
  const Code c = worst();
  return (c == Code::Count) ? "" : detail_[(uint8_t)c];
}

uint16_t Registry::raiseCount(Code c) const {
  const uint8_t i = (uint8_t)c;
  return (i < CODE_COUNT) ? counts_[i] : 0;
}

Registry& registry() { return g_registry; }

void raise(Code c, const char* detail) { g_registry.raise(c, detail); }
void clear(Code c) { g_registry.clear(c); }

}  // namespace fault
