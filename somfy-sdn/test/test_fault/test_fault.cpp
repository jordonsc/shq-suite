// Host tests for the device-level fault registry (fw 1.10.0, ledger shq-suite-0041).
//
// The property that matters: severity ordering. Bed 2's nine-hour clock wedge produced NO fault
// signal at all, and the fix is worthless if a trivial condition can mask the serious one.

#include <string.h>
#include <unity.h>

#include "fault.h"

void setUp() {}
void tearDown() {}

void test_starts_clear() {
  fault::Registry r;
  TEST_ASSERT_FALSE(r.any());
  TEST_ASSERT_EQUAL_STRING("ok", r.worstSlug());
  TEST_ASSERT_EQUAL_STRING("", r.worstDetail());
  TEST_ASSERT_TRUE(r.worst() == fault::Code::Count);
}

void test_raise_and_clear() {
  fault::Registry r;
  r.raise(fault::Code::BusOffline, "0/1 motors answering");
  TEST_ASSERT_TRUE(r.any());
  TEST_ASSERT_TRUE(r.active(fault::Code::BusOffline));
  TEST_ASSERT_FALSE(r.active(fault::Code::HeapLow));
  TEST_ASSERT_EQUAL_STRING("bus_offline", r.worstSlug());
  TEST_ASSERT_EQUAL_STRING("0/1 motors answering", r.worstDetail());

  r.clear(fault::Code::BusOffline);
  TEST_ASSERT_FALSE(r.any());
  TEST_ASSERT_EQUAL_STRING("ok", r.worstSlug());
}

// THE point of the ordering: a low-heap notice must never hide a dead clock.
void test_worst_is_most_severe_regardless_of_raise_order() {
  fault::Registry r;
  r.raise(fault::Code::HeapLow, "38 kB");
  TEST_ASSERT_EQUAL_STRING("heap_low", r.worstSlug());
  r.raise(fault::Code::ClockStalled, "pinned at 165947 s");
  TEST_ASSERT_EQUAL_STRING("clock_stalled", r.worstSlug());
  TEST_ASSERT_EQUAL_STRING("pinned at 165947 s", r.worstDetail());
  // Clearing the severe one falls back to what is still active, not to "ok".
  r.clear(fault::Code::ClockStalled);
  TEST_ASSERT_EQUAL_STRING("heap_low", r.worstSlug());
  TEST_ASSERT_EQUAL_STRING("38 kB", r.worstDetail());
}

// Raising is idempotent — the evaluator calls it every loop pass while the condition holds.
void test_raise_is_idempotent_but_counted() {
  fault::Registry r;
  for (int i = 0; i < 5; i++) r.raise(fault::Code::WsCapacity, "5/5 slots");
  TEST_ASSERT_EQUAL_UINT32(1u << (uint8_t)fault::Code::WsCapacity, r.mask());
  TEST_ASSERT_EQUAL_UINT16(5, r.raiseCount(fault::Code::WsCapacity));
}

// A later raise refreshes the detail, so the sensor shows the current magnitude, not the first.
void test_detail_refreshes() {
  fault::Registry r;
  r.raise(fault::Code::ClockRebase, "step -4294967 ms");
  r.raise(fault::Code::ClockRebase, "step -25769804 ms");
  TEST_ASSERT_EQUAL_STRING("step -25769804 ms", r.worstDetail());
}

// Detail longer than the buffer must truncate, not overrun.
void test_detail_truncates_safely() {
  fault::Registry r;
  char big[fault::DETAIL_MAX * 2];
  memset(big, 'x', sizeof(big) - 1);
  big[sizeof(big) - 1] = '\0';
  r.raise(fault::Code::HeapLow, big);
  TEST_ASSERT_EQUAL_UINT32(fault::DETAIL_MAX - 1, (uint32_t)strlen(r.worstDetail()));
}

void test_clear_all() {
  fault::Registry r;
  r.raise(fault::Code::ClockStalled);
  r.raise(fault::Code::HeapLow);
  r.clearAll();
  TEST_ASSERT_FALSE(r.any());
  TEST_ASSERT_EQUAL_UINT32(0, r.mask());
}

// Every code needs a slug and a description; a gap here shows up in HA as a blank sensor.
void test_every_code_has_text() {
  for (uint8_t i = 0; i < fault::CODE_COUNT; i++) {
    const char* s = fault::slug((fault::Code)i);
    const char* d = fault::describe((fault::Code)i);
    TEST_ASSERT_NOT_NULL(s);
    TEST_ASSERT_NOT_NULL(d);
    TEST_ASSERT_TRUE(strlen(s) > 0);
    TEST_ASSERT_TRUE(strlen(d) > 0);
    TEST_ASSERT_NOT_EQUAL(0, strcmp(s, "ok"));
  }
}

int main() {
  UNITY_BEGIN();
  RUN_TEST(test_starts_clear);
  RUN_TEST(test_raise_and_clear);
  RUN_TEST(test_worst_is_most_severe_regardless_of_raise_order);
  RUN_TEST(test_raise_is_idempotent_but_counted);
  RUN_TEST(test_detail_refreshes);
  RUN_TEST(test_detail_truncates_safely);
  RUN_TEST(test_clear_all);
  RUN_TEST(test_every_code_has_text);
  return UNITY_END();
}
