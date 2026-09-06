// Fault-tolerant monotonic millisecond clock (fw 1.10.0, ledger shq-suite-0041).
//
// WHY THIS EXISTS, AND WHY IT WAS REWRITTEN.
//
// The original (fw 1.5.1, ledger shq-suite-0034) defended against `millis()` occasionally
// returning a value FAR IN THE FUTURE, by sampling the clock twice and handing back the earlier
// of the pair. That defence was retired in 1.10.0 having never once fired: `clk_torn` read ZERO
// on all twelve somfy controllers and on the actron bridge, across four days of uptime each. It
// also failed to prevent the fault that did occur, because both reads of every pair returned the
// same wrong value. It now costs one clock read per call, not two — which matters in the bus
// task's tight deadline-poll loops and on the actron's timing-critical paths.
//
// What actually happened (Bed 2, 2026-08-31, ledger shq-suite-0041): `millis()` stepped BACKWARDS
// by exactly 6 x 2^32 microseconds and stayed there. The old filter's monotonic clamp
// (`if (delta < 0) return last_;`) then pinned `mono::now()` at a constant for 25,769 seconds —
// the precise size of the step — until the hardware clock climbed back to it. Every deadline in
// the firmware is `(uint32_t)(now - last) >= interval`, so with `now` frozen NOTHING timed ever
// fired again: no state push, no heartbeat, no RS485 poll. HA saw a device that accepted
// connections and sent nothing, and flapped `unavailable` on a 40 s cadence for nine hours.
//
// The clamp was right; its patience was not. A backward step made the size of a clock glitch into
// the length of a total outage.
//
// TWO DEFENCES, both host-tested in test_mono:
//
//   1. HIGH-WORD DETECTION. `esp_timer_get_time()` returns 64-bit microseconds and `millis()` is
//      that divided by 1000, so a fault in the counter's HIGH word moves `millis()` by a whole
//      multiple of 2^32 us (4,294,967.296 ms — see WORD_STEP_MS). Real elapsed time is never a
//      near-exact multiple of that: the loop calls `now()` continuously, so a genuine 71.6-minute
//      gap between two reads cannot happen. Such a step is therefore provably corrupt, in EITHER
//      direction, and is rejected on the first read rather than after the damage.
//
//   2. BOUNDED CLAMP. Rejection is capped at REBASE_AFTER_REJECTS consecutive samples. Past that
//      the clock has moved and stayed moved, and no amount of waiting will bring it back — so we
//      adopt it and record the step. Time going backwards once costs a single early deadline
//      firing (every gate is unsigned, so it fires immediately rather than wedging). Refusing to
//      adopt costs the entire device until the hardware catches up. That trade is not close.
//
// The counter that FOUND this fault is `backwardReads()`, and it is read as a RATE: a healthy
// controller accumulates a few hundred over four days (~0.002/s); thousands per second means the
// clock is pinned right now. Ledger shq-suite-0039 previously called it a meaningless artifact of
// this filter's documented data race and advised ignoring it — that guidance was wrong, was
// derived from a device that was itself pinned at the time, and is superseded by shq-suite-0041.

#pragma once

#include <cstdint>

namespace mono {

// One high-word unit of the 64-bit microsecond clock, in milliseconds: 2^32 us = 4,294,967.296 ms.
// The integer here is the truncation; wordStepUnits() does the exact arithmetic in 64-bit.
constexpr uint32_t WORD_STEP_MS = 4294967u;

// How far off an exact multiple of WORD_STEP_MS a step may land and still be called a high-word
// fault. Generous: the clock is sampled from two tasks, so a couple of seconds of slop is noise,
// and nothing legitimate lives anywhere near 71.6 minutes.
constexpr uint32_t WORD_STEP_TOLERANCE_MS = 2000u;

// Upper bound on the multiple we are willing to attribute to a high-word fault. 512 units is
// 25 days — beyond any plausible device uptime, and it keeps the 64-bit arithmetic well clear of
// overflowing the uint32_t magnitude it is compared against.
constexpr uint16_t WORD_STEP_MAX_UNITS = 512u;

// Consecutive rejected samples before we conclude the CLOCK moved rather than the sample being
// bad, and re-baseline onto it. The somfy bus task alone calls now() at kHz, so this is a
// sub-second window in practice. It must be far above any run of backward reads the documented
// inter-task race can produce: that race yields ISOLATED backward samples (the healthy fleet
// floor is a few hundred in four days), and any forward read resets the run to zero.
constexpr uint32_t REBASE_AFTER_REJECTS = 1000u;

// Forward steps larger than this are recorded as jumps — and still ACCEPTED. A genuine
// multi-second stall happens (OTA download, a long bus transaction), and freezing time would be
// worse than the disease. Only the high-word test rejects a forward step.
constexpr uint32_t JUMP_THRESHOLD_MS = 60000;

// Pure, Arduino-free filter so the logic is host-testable (`pio test -e native`, test_mono).
//
// Not internally synchronised: the shared instance behind `now()` is read/written from both the
// Arduino loop and the bus task, but every field is a naturally-aligned uint32_t (atomic on
// RV32), so the worst a race can do is lose a counter increment or let one sample through
// unclamped. That is cheaper and far safer than disabling interrupts in the bus hot path.
class Filter {
 public:
  // Feed one raw sample of the underlying clock; returns the filtered time.
  uint32_t step(uint32_t t);

  // Whole 2^32-microsecond units in `magnitude_ms`, or 0 if it is not a near-exact multiple.
  // Direction-agnostic — callers pass a magnitude. Exposed for testing.
  static uint16_t wordStepUnits(uint32_t magnitude_ms);

  uint32_t last() const { return last_; }
  uint32_t backwardReads() const { return back_; }      // samples older than the last accepted one
  uint32_t wordSteps() const { return word_steps_; }    // rejected high-word faults
  uint16_t lastWordUnits() const { return last_word_units_; }  // size of the newest, in units
  uint32_t rebases() const { return rebases_; }         // clamps abandoned; the clock had moved
  int32_t lastRebaseMs() const { return last_rebase_ms_; }     // signed size of the adopted step
  uint32_t forwardJumps() const { return jumps_; }      // accepted steps > JUMP_THRESHOLD_MS
  uint32_t lastJumpMs() const { return last_jump_ms_; }

 private:
  uint32_t last_ = 0;
  uint32_t back_ = 0;
  uint32_t word_steps_ = 0;
  uint32_t rebases_ = 0;
  uint32_t jumps_ = 0;
  uint32_t last_jump_ms_ = 0;
  uint32_t consec_rejects_ = 0;
  int32_t last_rebase_ms_ = 0;
  uint16_t last_word_units_ = 0;
  bool started_ = false;
};

// Filtered `millis()`. Use this for every deadline, timestamp and age calculation.
uint32_t now();

// Elapsed time between two now() reads taken in that order, clamped at zero. A re-baseline
// (layer 2 above) between the two reads can make `to` smaller than `from`; the bare unsigned
// difference then reads as ~49.7 days, which is how Bed 4 logged a 4,294,963,989 ms loop stall
// (ledger shq-suite-0049). Use this for any duration measured across two reads in one pass —
// NOT for deadline checks, where the unsigned `(now - last) >= interval` form is the right one.
inline uint32_t elapsed(uint32_t from, uint32_t to) {
  const int32_t d = (int32_t)(to - from);
  return d < 0 ? 0u : (uint32_t)d;
}

// Telemetry (surfaced as clk_back/clk_word/clk_rebase/clk_jump in /stats, the health push and
// the HA clock sensors, and as a `clock_glitch` diag record). A non-zero word/rebase count is
// this module doing its job — the clock misbehaved and the device kept running anyway. Neither is
// a fault (fw 1.14.3): handled means nothing to respond to.
uint32_t backwardReads();
uint32_t wordSteps();
uint16_t lastWordUnits();
uint32_t rebases();
int32_t lastRebaseMs();
uint32_t forwardJumps();
uint32_t lastJumpMs();

}  // namespace mono
