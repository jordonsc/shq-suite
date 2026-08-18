#include "mono.h"

namespace mono {

uint32_t Filter::step(uint32_t a, uint32_t b) {
  // Normal case: `b` was read a hair after `a`, so the spread is 0–1 ms and we take `a`. A
  // glitched read lands far from its partner: if the SECOND sample is the wild one the spread is
  // hugely positive and `a` is still the sane value; if the FIRST is wild the spread goes
  // negative and `b` is the sane one. Either way, take the earlier sample.
  int32_t spread = (int32_t)(b - a);
  uint32_t t = (spread < 0) ? b : a;
  if (spread < 0 || (uint32_t)spread > TORN_TOLERANCE_MS) torn_++;

  if (!started_) {
    started_ = true;
    last_ = t;
    return t;
  }

  int32_t delta = (int32_t)(t - last_);
  if (delta < 0) {
    // Never hand out time that runs backwards — a caller computing `deadline = now + timeout`
    // would otherwise get a deadline in the past and spin.
    back_++;
    return last_;
  }
  if ((uint32_t)delta > JUMP_THRESHOLD_MS) {
    jumps_++;
    last_jump_ms_ = (uint32_t)delta;
  }
  last_ = t;
  return t;
}

#ifdef ARDUINO

}  // namespace mono

#include <Arduino.h>

namespace mono {

namespace {
Filter g_filter;
}  // namespace

uint32_t now() {
  uint32_t a = ::millis();
  uint32_t b = ::millis();
  return g_filter.step(a, b);
}

uint32_t tornReads() { return g_filter.tornReads(); }
uint32_t backwardReads() { return g_filter.backwardReads(); }
uint32_t forwardJumps() { return g_filter.forwardJumps(); }
uint32_t lastJumpMs() { return g_filter.lastJumpMs(); }

#endif  // ARDUINO

}  // namespace mono
