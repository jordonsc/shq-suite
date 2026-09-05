// Host tests for the MAC transmit-statistics accumulator (fw 1.12.0, txstats.h).
//
// The driver's counters are read and cleared once a second; what has to be right is the
// bookkeeping on top: lifetime totals sum, the two consumer windows (health push / diag record)
// are independent, maxima are carried rather than added, and a counter that went backwards
// (cleared under us) is taken whole rather than wrapped.

#include <string.h>
#include <unity.h>

#include "tcpsnap.h"
#include "txstats.h"

using txstats::Accumulator;
using txstats::Counters;

void setUp() {}
void tearDown() {}

static Counters reading(uint32_t ok, uint32_t retry, uint32_t to, uint32_t rtt) {
  Counters c;
  c.tx_succ = ok;
  c.retry_edca = retry;
  c.timeout = to;
  c.seq_max_rtt_us = rtt;
  return c;
}

void test_totals_sum_and_max_carries() {
  Accumulator a;
  a.add(reading(10, 2, 1, 500));
  a.add(reading(5, 3, 0, 300));
  TEST_ASSERT_EQUAL_UINT32(15, a.total().tx_succ);
  TEST_ASSERT_EQUAL_UINT32(5, a.total().retries());
  TEST_ASSERT_EQUAL_UINT32(1, a.total().timeout);
  TEST_ASSERT_EQUAL_UINT32(500, a.total().seq_max_rtt_us);  // max, not 800
}

void test_windows_are_independent() {
  Accumulator a;
  a.add(reading(10, 2, 1, 0));
  a.markHealth();
  a.add(reading(4, 1, 1, 0));
  // Health window sees only the second reading; the record window has never been marked and
  // therefore sees everything.
  TEST_ASSERT_EQUAL_UINT32(4, a.sinceHealth().tx_succ);
  TEST_ASSERT_EQUAL_UINT32(1, a.sinceHealth().retries());
  TEST_ASSERT_EQUAL_UINT32(14, a.sinceRecord().tx_succ);
  TEST_ASSERT_EQUAL_UINT32(3, a.sinceRecord().retries());
  a.markRecord();
  TEST_ASSERT_EQUAL_UINT32(0, a.sinceRecord().tx_succ);
  TEST_ASSERT_EQUAL_UINT32(4, a.sinceHealth().tx_succ);  // untouched by the record mark
}

void test_delta_takes_reset_whole() {
  TEST_ASSERT_EQUAL_UINT32(5, txstats::deltaFrom(10, 15));
  TEST_ASSERT_EQUAL_UINT32(0, txstats::deltaFrom(10, 10));
  // A cleared counter reads lower than before: that is 3 new events, not 4 billion.
  TEST_ASSERT_EQUAL_UINT32(3, txstats::deltaFrom(10, 3));
}

void test_since_never_wraps() {
  Counters base = reading(100, 20, 5, 900);
  Counters cur = reading(90, 25, 5, 400);  // tx_succ went backwards, retries forwards
  Counters d = cur.since(base);
  TEST_ASSERT_EQUAL_UINT32(90, d.tx_succ);
  TEST_ASSERT_EQUAL_UINT32(5, d.retries());
  TEST_ASSERT_EQUAL_UINT32(0, d.timeout);
  TEST_ASSERT_EQUAL_UINT32(400, d.seq_max_rtt_us);  // current max, never a difference
}

void test_fresh_accumulator_is_zero() {
  Accumulator a;
  TEST_ASSERT_EQUAL_UINT32(0, a.total().tx_succ);
  TEST_ASSERT_EQUAL_UINT32(0, a.sinceHealth().retries());
  TEST_ASSERT_EQUAL_UINT32(0, a.sinceRecord().timeout);
}

void test_tcp_state_names() {
  TEST_ASSERT_EQUAL_STRING("established", tcpsnap::stateName(4));
  TEST_ASSERT_EQUAL_STRING("close_wait", tcpsnap::stateName(7));
  TEST_ASSERT_EQUAL_STRING("?", tcpsnap::stateName(42));
  tcpsnap::Snap s;
  TEST_ASSERT_EQUAL_UINT8(0, s.valid);  // a default snapshot is "no pcb found"
}


void test_mapped_v4_unmaps_and_keeps_network_order() {
  // ::ffff:192.0.2.5 as lwIP hands it back from getpeername() on a dual-stack listener.
  const uint8_t mapped[16] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0xFF, 192, 0, 2, 5};
  uint32_t v4 = 0;
  TEST_ASSERT_TRUE(tcpsnap::mappedV4(mapped, &v4));
  uint8_t b[4];
  memcpy(b, &v4, 4);  // network order: the bytes come out in address order on any host
  TEST_ASSERT_EQUAL_UINT8(192, b[0]);
  TEST_ASSERT_EQUAL_UINT8(0, b[1]);
  TEST_ASSERT_EQUAL_UINT8(2, b[2]);
  TEST_ASSERT_EQUAL_UINT8(5, b[3]);
}

void test_mapped_v4_rejects_real_v6_and_bad_prefix() {
  uint32_t v4 = 0xDEADBEEF;
  const uint8_t ll[16] = {0xFE, 0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1};  // fe80::1
  TEST_ASSERT_FALSE(tcpsnap::mappedV4(ll, &v4));
  const uint8_t compat[16] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 192, 0, 2, 5};  // ::192.0.2.5
  TEST_ASSERT_FALSE(tcpsnap::mappedV4(compat, &v4));
  const uint8_t half[16] = {0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xFF, 0, 192, 0, 2, 5};
  TEST_ASSERT_FALSE(tcpsnap::mappedV4(half, &v4));
  TEST_ASSERT_EQUAL_UINT32(0xDEADBEEF, v4);  // untouched on rejection
  TEST_ASSERT_FALSE(tcpsnap::mappedV4(nullptr, &v4));
  TEST_ASSERT_FALSE(tcpsnap::mappedV4(ll, nullptr));
}

int main() {
  UNITY_BEGIN();
  RUN_TEST(test_totals_sum_and_max_carries);
  RUN_TEST(test_windows_are_independent);
  RUN_TEST(test_delta_takes_reset_whole);
  RUN_TEST(test_since_never_wraps);
  RUN_TEST(test_fresh_accumulator_is_zero);
  RUN_TEST(test_tcp_state_names);
  RUN_TEST(test_mapped_v4_unmaps_and_keeps_network_order);
  RUN_TEST(test_mapped_v4_rejects_real_v6_and_bad_prefix);
  return UNITY_END();
}
