#include "mono.h"

namespace mono {

uint16_t Filter::wordStepUnits(uint32_t magnitude_ms) {
  // Cheap reject first: anything below one unit (minus slop) cannot be a high-word fault, and
  // that is every ordinary sample.
  if (magnitude_ms < WORD_STEP_MS - WORD_STEP_TOLERANCE_MS) return 0;

  const uint32_t n = (magnitude_ms + WORD_STEP_MS / 2) / WORD_STEP_MS;
  if (n == 0 || n > WORD_STEP_MAX_UNITS) return 0;

  // Exact position of n units, in milliseconds, without accumulating the truncation error of
  // WORD_STEP_MS: n * 2^32 microseconds / 1000. Caps at 512 units so this stays inside uint64.
  const uint64_t exact_ms = ((uint64_t)n * 4294967296ull) / 1000ull;
  const uint64_t mag = (uint64_t)magnitude_ms;
  const uint64_t diff = (mag > exact_ms) ? (mag - exact_ms) : (exact_ms - mag);
  return (diff <= WORD_STEP_TOLERANCE_MS) ? (uint16_t)n : 0;
}

uint32_t Filter::step(uint32_t t) {
  if (!started_) {
    started_ = true;
    last_ = t;
    return t;
  }

  // Signed for direction, unsigned for magnitude — both wrap correctly across the 49.7-day
  // millis() rollover, which presents as an ordinary small forward step.
  const int32_t delta = (int32_t)(t - last_);
  const uint32_t magnitude = (delta < 0) ? (uint32_t)(last_ - t) : (uint32_t)(t - last_);

  bool reject = false;
  const uint16_t units = wordStepUnits(magnitude);
  if (units != 0) {
    // A whole number of 2^32-us units, in either direction: the high word of the microsecond
    // counter is wrong. Nothing legitimate moves the clock by 71.6 minutes between two reads.
    word_steps_++;
    last_word_units_ = units;
    reject = true;
  } else if (delta < 0) {
    // Never hand out time that runs backwards on the strength of one sample — a caller computing
    // `deadline = now + timeout` would get a deadline already in the past and spin.
    back_++;
    reject = true;
  }

  if (reject) {
    if (++consec_rejects_ < REBASE_AFTER_REJECTS) return last_;
    // Out of patience. The clock has read wrong this many times in a row, so it is the clock that
    // moved, not the sample that was bad, and holding `last_` any longer freezes every deadline in
    // the firmware for as long as the step is large (ledger shq-suite-0041: nine hours). Adopt it.
    rebases_++;
    last_rebase_ms_ = delta;
    consec_rejects_ = 0;
    last_ = t;
    return t;
  }

  consec_rejects_ = 0;
  if (magnitude > JUMP_THRESHOLD_MS) {
    jumps_++;
    last_jump_ms_ = magnitude;
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

uint32_t now() { return g_filter.step(::millis()); }

uint32_t backwardReads() { return g_filter.backwardReads(); }
uint32_t wordSteps() { return g_filter.wordSteps(); }
uint16_t lastWordUnits() { return g_filter.lastWordUnits(); }
uint32_t rebases() { return g_filter.rebases(); }
int32_t lastRebaseMs() { return g_filter.lastRebaseMs(); }
uint32_t forwardJumps() { return g_filter.forwardJumps(); }
uint32_t lastJumpMs() { return g_filter.lastJumpMs(); }

#endif  // ARDUINO

}  // namespace mono
