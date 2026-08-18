// Glitch-filtered monotonic millisecond clock (fw 1.5.1, ledger shq-suite-0034).
//
// Why this exists: `millis()` on these C6 boards occasionally returns a value FAR in the future.
// The Bed 2 controller's error ring held six entries stamped up to 780,083 s on a device only
// 31,300 s into its boot — RAM-only ring, so those stamps came from live `millis()` calls. One
// such read landing in a deadline variable poisons it: `last = <far future>` makes every
// `now - last` comparison read as "not due yet" for as long as the bogus offset lasts (hours to
// days). That is exactly how the WS state heartbeat wedged, which HA sees as a device that
// accepts connections but never sends state — the "silent socket" (shq-suite-0019/0022).
//
// Two defences, applied together:
//   1. `mono::now()` — sample the clock twice and hand back the EARLIER of the pair. A glitch is
//      a transient single bad read, so its partner is sane; taking the earlier sample discards it
//      without ever freezing the clock. The result is then clamped monotonic.
//   2. Deadline comparisons switched to UNSIGNED arithmetic at the call sites (see ws_api.cpp).
//      Should a bogus value get stored anyway, `(uint32_t)(now - last) >= interval` wraps to a
//      huge number and fires on the very next loop instead of wedging.
//
// Big forward steps are counted but NOT suppressed: a genuine multi-second stall happens (OTA
// download, a long bus transaction), and freezing time would be worse than the disease.

#pragma once

#include <cstdint>

namespace mono {

// A pair of back-to-back reads normally differs by 0–1 ms. Beyond this they disagree by more than
// scheduling noise can explain, so one of them is a glitch.
constexpr uint32_t TORN_TOLERANCE_MS = 250;

// Forward steps larger than this are recorded as jumps (still accepted — see the note above).
constexpr uint32_t JUMP_THRESHOLD_MS = 60000;

// Pure, Arduino-free filter so the logic is host-testable (`pio test -e native`, test_mono).
// Not internally synchronised: the shared instance behind `now()` is read/written from both the
// Arduino loop and the bus task, but every field is a naturally-aligned uint32_t (atomic on
// RV32), so the worst a race can do is lose a counter increment or let one sample through
// unclamped. That is cheaper and far safer than disabling interrupts in the bus hot path.
class Filter {
 public:
  // Feed two consecutive raw samples of the underlying clock; returns the filtered time.
  uint32_t step(uint32_t a, uint32_t b);

  uint32_t last() const { return last_; }
  uint32_t tornReads() const { return torn_; }        // sample pairs that disagreed
  uint32_t backwardReads() const { return back_; }    // samples older than the last accepted one
  uint32_t forwardJumps() const { return jumps_; }    // accepted steps > JUMP_THRESHOLD_MS
  uint32_t lastJumpMs() const { return last_jump_ms_; }

 private:
  uint32_t last_ = 0;
  uint32_t torn_ = 0;
  uint32_t back_ = 0;
  uint32_t jumps_ = 0;
  uint32_t last_jump_ms_ = 0;
  bool started_ = false;
};

// Filtered `millis()`. Use this for every deadline, timestamp and age calculation.
uint32_t now();

// Telemetry (surfaced as clk_torn/clk_back/clk_jump in /stats) — a non-zero torn/jump count is
// the fingerprint of the glitch this module exists to absorb.
uint32_t tornReads();
uint32_t backwardReads();
uint32_t forwardJumps();
uint32_t lastJumpMs();

}  // namespace mono
