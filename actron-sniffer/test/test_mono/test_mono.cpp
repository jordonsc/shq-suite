// Host tests for the glitch-filtered monotonic clock (ledger shq-suite-0034).
// Twin of somfy-sdn/test/test_mono — keep the two in step.
//
// The field failure these guard against: a single far-future millis() read stamped into a
// deadline variable, after which every "is it due yet?" test says no for hours. The filter's job
// is to drop that read without ever freezing or rewinding the clock.

#include <unity.h>

#include "mono.h"

void setUp() {}
void tearDown() {}

// Ordinary progression: two reads a hair apart, time advances.
void test_normal_progression() {
  mono::Filter f;
  TEST_ASSERT_EQUAL_UINT32(1000, f.step(1000, 1000));
  TEST_ASSERT_EQUAL_UINT32(1005, f.step(1005, 1006));
  TEST_ASSERT_EQUAL_UINT32(2000, f.step(2000, 2000));
  TEST_ASSERT_EQUAL_UINT32(0, f.tornReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.backwardReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.forwardJumps());
}

// The Bed 2 signature: the SECOND sample comes back hours in the future. The sane first sample
// must win, and the excursion must be counted.
void test_second_sample_glitches_far_future() {
  mono::Filter f;
  f.step(1000, 1000);
  TEST_ASSERT_EQUAL_UINT32(1010, f.step(1010, 780083572u));
  TEST_ASSERT_EQUAL_UINT32(1, f.tornReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.forwardJumps());  // the bad value never reached the clock
  TEST_ASSERT_EQUAL_UINT32(1020, f.step(1020, 1020));
}

// Same glitch, but it lands in the FIRST sample — then the second one is the sane value.
void test_first_sample_glitches_far_future() {
  mono::Filter f;
  f.step(1000, 1000);
  TEST_ASSERT_EQUAL_UINT32(1010, f.step(780083572u, 1010));
  TEST_ASSERT_EQUAL_UINT32(1, f.tornReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.forwardJumps());
}

// A glitch that reads LOW must not rewind the clock: callers compute `deadline = now + timeout`,
// and a rewound now yields a deadline that has already passed.
void test_never_runs_backwards() {
  mono::Filter f;
  f.step(50000, 50000);
  TEST_ASSERT_EQUAL_UINT32(50000, f.step(10, 12));
  TEST_ASSERT_EQUAL_UINT32(1, f.backwardReads());
  TEST_ASSERT_EQUAL_UINT32(50010, f.step(50010, 50010));
}

// A genuine long stall (OTA download, a slow bus transaction) must be ACCEPTED, just recorded —
// suppressing it would freeze time, which is worse than the bug being fixed.
void test_genuine_long_stall_is_accepted() {
  mono::Filter f;
  f.step(1000, 1000);
  TEST_ASSERT_EQUAL_UINT32(121000, f.step(121000, 121000));
  TEST_ASSERT_EQUAL_UINT32(1, f.forwardJumps());
  TEST_ASSERT_EQUAL_UINT32(120000, f.lastJumpMs());
}

// Scheduling noise between the two reads is not a glitch.
void test_small_spread_is_not_torn() {
  mono::Filter f;
  f.step(1000, 1000);
  f.step(2000, 2000 + mono::TORN_TOLERANCE_MS);
  TEST_ASSERT_EQUAL_UINT32(0, f.tornReads());
  f.step(3000, 3000 + mono::TORN_TOLERANCE_MS + 1);
  TEST_ASSERT_EQUAL_UINT32(1, f.tornReads());
}

// The 49.7-day millis() wrap must keep working: a wrapped sample is a small forward step in
// modular arithmetic, not a rewind.
void test_wraparound_is_forward() {
  mono::Filter f;
  const uint32_t near_wrap = 0xFFFFFF00u;
  f.step(near_wrap, near_wrap);
  TEST_ASSERT_EQUAL_UINT32(0x40, f.step(0x40, 0x40));  // wrapped past zero
  TEST_ASSERT_EQUAL_UINT32(0, f.backwardReads());
  TEST_ASSERT_EQUAL_UINT32(0, f.forwardJumps());
}

int main() {
  UNITY_BEGIN();
  RUN_TEST(test_normal_progression);
  RUN_TEST(test_second_sample_glitches_far_future);
  RUN_TEST(test_first_sample_glitches_far_future);
  RUN_TEST(test_never_runs_backwards);
  RUN_TEST(test_genuine_long_stall_is_accepted);
  RUN_TEST(test_small_spread_is_not_torn);
  RUN_TEST(test_wraparound_is_forward);
  return UNITY_END();
}
