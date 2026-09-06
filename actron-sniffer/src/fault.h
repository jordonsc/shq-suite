// Device-level fault registry (fw 1.10.0, ledger shq-suite-0041).
//
// WHY: when Bed 2's clock wedged for nine hours, every fault signal the estate had stayed clear.
// `binary_sensor.<motor>_fault` reports the SDN motor's own status word and knew nothing about
// the controller hosting it; the controller had no fault concept at all. The failure was found
// because a human noticed a blind not responding.
//
// A boolean cannot carry "which fault", so this registry is a small bitmask of named conditions,
// each with a short slug and a one-line detail string filled in at raise time. The WS health push
// and `/stats` carry the worst active code plus its detail; the HA component turns that into
// `sensor.<controller>_fault` (state = slug, "ok" when clear) and a derived
// `binary_sensor.<controller>_problem` for automations to trigger on.
//
// Codes are declared in SEVERITY ORDER — `worst()` returns the lowest-ordinal active code, so a
// dead clock is never masked by a low-heap notice.
//
// Pure and Arduino-free so the logic is host-testable (`pio test -e native`, test_fault). The
// evaluator that decides when to raise each code lives in main.cpp, where the live inputs are.

#pragma once

#include <cstdint>

namespace fault {

enum class Code : uint8_t {
  ClockStalled = 0,  // mono::now() has not advanced across many loop iterations — device is dead
  BusOffline,        // motors are configured but none are answering the RS485 bus
  WsCapacity,        // every WS client slot occupied; the HA coordinator cannot get in
  HeapLow,           // free heap below the floor a reconnect needs
  // NOT faults (removed fw 1.14.3, ledger shq-suite-0050): a clock re-baseline or a rejected
  // high-word step. Both are the filter doing its job — handled, nothing for anyone to do — and
  // a latched Problem for a handled event is noise. They stay fully logged: the `clock_glitch`
  // diag record (a logbook line in HA), the clk_word / clk_rebase / clk_rebase_ms counters in
  // /stats and the health push, and the HA "Clock re-baselines" / "Clock high-word faults"
  // sensors. A fault here is for a condition that needs a response.
  Count
};

constexpr uint8_t CODE_COUNT = (uint8_t)Code::Count;

// Longest detail string retained per code, including the terminator. Kept small deliberately:
// this is a one-line "what and how big", not a log.
constexpr uint8_t DETAIL_MAX = 56;

// Stable machine-readable slug, e.g. "clock_stalled". Never null; "ok" for no fault.
const char* slug(Code c);

// Fixed one-line human description of the condition.
const char* describe(Code c);

class Registry {
 public:
  // Idempotent: re-raising an active code refreshes its detail and bumps its count, but does not
  // re-trigger. `detail` may be null.
  void raise(Code c, const char* detail = nullptr);
  void clear(Code c);
  void clearAll();

  bool active(Code c) const;
  bool any() const { return mask_ != 0; }
  uint32_t mask() const { return mask_; }

  // Lowest-ordinal active code, i.e. the most severe. Returns Code::Count when clear.
  Code worst() const;

  // Slug of the worst active code, or "ok".
  const char* worstSlug() const;
  // Detail of the worst active code, or "" when clear / no detail was given.
  const char* worstDetail() const;

  uint16_t raiseCount(Code c) const;

 private:
  uint32_t mask_ = 0;
  uint16_t counts_[CODE_COUNT] = {};
  char detail_[CODE_COUNT][DETAIL_MAX] = {};
};

// The device-wide instance.
Registry& registry();

// Convenience wrappers so call sites read as `fault::raise(...)`.
void raise(Code c, const char* detail = nullptr);
void clear(Code c);

}  // namespace fault
