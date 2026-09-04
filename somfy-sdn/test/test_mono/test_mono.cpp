// Host tests for the fault-tolerant monotonic clock (fw 1.10.0, ledger shq-suite-0041).
//
// The field failure these guard against: `millis()` stepped backwards by exactly 6 x 2^32
// microseconds and stayed there, and the old filter's unbounded monotonic clamp then pinned
// mono::now() for the 25,769 seconds it took the hardware clock to climb back — nine hours during
// which no deadline in the firmware fired at all. Two things must hold now: a high-word step is
// recognised and rejected on the FIRST read, and no clamp may last for ever.

#include <unity.h>

#include "mono.h"

void setUp() {}
void tearDown() {}

// The Bed 2 signature, in milliseconds: 6 x 2^32 us.
static constexpr uint32_t SIX_UNITS_MS = 25769804u;  // 6 * 4294967.296, rounded

// Ordinary progression: time advances, nothing is flagged.
void test_normal_progression() {
  mono::Filter f;
  TEST_ASSERT_EQUAL_UINT32(1000, f.step(1000));
  TEST_ASSERT_EQUAL_UINT32(1005, f.step(1005));
  TEST_ASSERT_EQUAL_UINT32(2000, f.step(2000));
  TEST_ASSERT_EQUAL_UINT32(0, f.backwardReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.wordSteps());
  TEST_ASSERT_EQUAL_UINT32(0, f.rebases());
  TEST_ASSERT_EQUAL_UINT32(0, f.forwardJumps());
}

// wordStepUnits is the whole detector; test it directly at the boundaries.
void test_word_step_units_recognises_exact_multiples() {
  TEST_ASSERT_EQUAL_UINT16(1, mono::Filter::wordStepUnits(4294967u));
  TEST_ASSERT_EQUAL_UINT16(6, mono::Filter::wordStepUnits(SIX_UNITS_MS));
  TEST_ASSERT_EQUAL_UINT16(2, mono::Filter::wordStepUnits(8589935u));
  // Within tolerance either side of an exact multiple.
  TEST_ASSERT_EQUAL_UINT16(1, mono::Filter::wordStepUnits(4294967u + 1999u));
  TEST_ASSERT_EQUAL_UINT16(1, mono::Filter::wordStepUnits(4294967u - 1999u));
}

void test_word_step_units_rejects_everything_else() {
  TEST_ASSERT_EQUAL_UINT16(0, mono::Filter::wordStepUnits(0));
  TEST_ASSERT_EQUAL_UINT16(0, mono::Filter::wordStepUnits(120000u));       // a genuine long stall
  TEST_ASSERT_EQUAL_UINT16(0, mono::Filter::wordStepUnits(2147483u));      // half a unit
  TEST_ASSERT_EQUAL_UINT16(0, mono::Filter::wordStepUnits(4294967u + 5000u));   // near, not near enough
  TEST_ASSERT_EQUAL_UINT16(0, mono::Filter::wordStepUnits(SIX_UNITS_MS + 9000u));
  // Beyond WORD_STEP_MAX_UNITS we decline to claim the pattern.
  TEST_ASSERT_EQUAL_UINT16(0, mono::Filter::wordStepUnits(4294967u * 600u));
}

// THE regression test: the exact Bed 2 fault. One backward high-word step must be rejected on
// sight, leaving the clock untouched, and must be counted as a word step rather than a plain
// backward read.
void test_backward_high_word_step_is_rejected_on_first_read() {
  mono::Filter f;
  f.step(165947000u);
  TEST_ASSERT_EQUAL_UINT32(165947000u, f.step(165947000u - SIX_UNITS_MS));
  TEST_ASSERT_EQUAL_UINT32(1, f.wordSteps());
  TEST_ASSERT_EQUAL_UINT16(6, f.lastWordUnits());
  TEST_ASSERT_EQUAL_UINT32(0, f.backwardReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.rebases());
  // A good sample straight after is accepted normally.
  TEST_ASSERT_EQUAL_UINT32(165947010u, f.step(165947010u));
}

// The same fault forwards. Large forward steps are normally ACCEPTED (a real stall), so the
// high-word test is the only thing standing between us and a poisoned clock.
void test_forward_high_word_step_is_rejected() {
  mono::Filter f;
  f.step(50000u);
  TEST_ASSERT_EQUAL_UINT32(50000u, f.step(50000u + SIX_UNITS_MS));
  TEST_ASSERT_EQUAL_UINT32(1, f.wordSteps());
  TEST_ASSERT_EQUAL_UINT32(0, f.forwardJumps());  // never reached the clock, so never a "jump"
  TEST_ASSERT_EQUAL_UINT32(50010u, f.step(50010u));
}

// A high-word fault that PERSISTS is the real Bed 2 case: every subsequent read is also a
// word step. Rejecting for ever would reproduce the nine-hour wedge, so the clamp must give up
// and adopt the clock the device actually has.
void test_persistent_high_word_step_rebases() {
  mono::Filter f;
  const uint32_t base = 165947000u;
  f.step(base);
  uint32_t out = 0;
  for (uint32_t i = 0; i < mono::REBASE_AFTER_REJECTS; i++) out = f.step(base - SIX_UNITS_MS + i);
  // The last of those crossed the budget and was adopted.
  TEST_ASSERT_EQUAL_UINT32(base - SIX_UNITS_MS + mono::REBASE_AFTER_REJECTS - 1, out);
  TEST_ASSERT_EQUAL_UINT32(1, f.rebases());
  TEST_ASSERT_TRUE(f.lastRebaseMs() < 0);
  // And from there it tracks normally again, which is the whole point.
  TEST_ASSERT_EQUAL_UINT32(out + 10, f.step(out + 10));
}

// A glitch that reads LOW must not rewind the clock on one sample: callers compute
// `deadline = now + timeout`, and a rewound now yields a deadline that has already passed.
void test_single_backward_read_is_clamped() {
  mono::Filter f;
  f.step(50000);
  TEST_ASSERT_EQUAL_UINT32(50000, f.step(10));
  TEST_ASSERT_EQUAL_UINT32(1, f.backwardReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.rebases());
  TEST_ASSERT_EQUAL_UINT32(50010, f.step(50010));
}

// Sustained backward reads that are NOT a high-word multiple must still re-baseline: whatever
// broke the clock, a permanently frozen mono::now() is never the right answer.
void test_sustained_backward_reads_rebase() {
  mono::Filter f;
  f.step(50000);
  uint32_t out = 0;
  for (uint32_t i = 0; i < mono::REBASE_AFTER_REJECTS; i++) out = f.step(10 + i);
  TEST_ASSERT_EQUAL_UINT32(10 + mono::REBASE_AFTER_REJECTS - 1, out);
  TEST_ASSERT_EQUAL_UINT32(1, f.rebases());
  // Every one of them counted, including the sample that finally got adopted.
  TEST_ASSERT_EQUAL_UINT32(mono::REBASE_AFTER_REJECTS, f.backwardReads());
}

// The benign inter-task race produces ISOLATED backward samples, never a run of them. Any
// forward read must reset the budget, or a healthy controller would eventually re-baseline off
// nothing more than scheduling noise.
void test_isolated_backward_reads_never_rebase() {
  mono::Filter f;
  f.step(50000);
  for (uint32_t i = 0; i < mono::REBASE_AFTER_REJECTS * 3; i++) {
    f.step(49999);        // a stale sample from the other task
    f.step(50000 + i);    // ... immediately followed by a good one
  }
  TEST_ASSERT_EQUAL_UINT32(0, f.rebases());
  TEST_ASSERT_TRUE(f.backwardReads() > 0);
}

// A genuine long stall (OTA download, a slow bus transaction) must be ACCEPTED, just recorded —
// suppressing it would freeze time, which is worse than the bug being fixed.
void test_genuine_long_stall_is_accepted() {
  mono::Filter f;
  f.step(1000);
  TEST_ASSERT_EQUAL_UINT32(121000, f.step(121000));
  TEST_ASSERT_EQUAL_UINT32(1, f.forwardJumps());
  TEST_ASSERT_EQUAL_UINT32(120000, f.lastJumpMs());
  TEST_ASSERT_EQUAL_UINT32(0, f.wordSteps());
}

// The 49.7-day millis() wrap must keep working: a wrapped sample is a small forward step in
// modular arithmetic, not a rewind and not a high-word fault.
void test_wraparound_is_forward() {
  mono::Filter f;
  const uint32_t near_wrap = 0xFFFFFF00u;
  f.step(near_wrap);
  TEST_ASSERT_EQUAL_UINT32(0x40, f.step(0x40));  // wrapped past zero
  TEST_ASSERT_EQUAL_UINT32(0, f.backwardReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.wordSteps());
  TEST_ASSERT_EQUAL_UINT32(0, f.forwardJumps());
}

int main() {
  UNITY_BEGIN();
  RUN_TEST(test_normal_progression);
  RUN_TEST(test_word_step_units_recognises_exact_multiples);
  RUN_TEST(test_word_step_units_rejects_everything_else);
  RUN_TEST(test_backward_high_word_step_is_rejected_on_first_read);
  RUN_TEST(test_forward_high_word_step_is_rejected);
  RUN_TEST(test_persistent_high_word_step_rebases);
  RUN_TEST(test_single_backward_read_is_clamped);
  RUN_TEST(test_sustained_backward_reads_rebase);
  RUN_TEST(test_isolated_backward_reads_never_rebase);
  RUN_TEST(test_genuine_long_stall_is_accepted);
  RUN_TEST(test_wraparound_is_forward);
  return UNITY_END();
}
