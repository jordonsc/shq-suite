// Host tests for the WS client liveness policy (fw 1.14.0, ws_liveness.h, ledger shq-suite-0046).
//
// Each case is one line of the pcap that settled the investigation:
//   * a 25 s uplink hole with lwIP retransmitting must NOT cost the session (the old library
//     reaper evicted at 10 s after the first missed pong, i.e. after three retransmissions);
//   * a peer silent in both directions for 45 s must go, but only after HA's own 40 s window;
//   * a silent peer behind a retransmitting pcb gets an extension, bounded at 120 s;
//   * HA's inbound pings alone keep a socket alive even when our pongs are stuck;
//   * a young socket is never judged; an unwritable one is reaped at 30 s regardless.

#include <unity.h>

#include "ws_liveness.h"

using ws_liveness::Input;
using ws_liveness::Result;
using ws_liveness::Verdict;

void setUp() {}
void tearDown() {}

// A healthy client `age` ms old whose last inbound frame was `silence` ms ago.
static Input client(uint32_t age, uint32_t silence, bool retransmitting = false, uint32_t unwritable = 0) {
  Input in{};
  in.age_ms = age;
  in.pong_age_ms = silence;
  in.rx_age_ms = silence;
  in.unwritable_ms = unwritable;
  in.retransmitting = retransmitting;
  return in;
}

static void test_ladder_arithmetic_matches_the_capture() {
  // 1.2, 3.6, 8.4, 18.0, 37.2, 75.6 s — the measured backoff on these sockets.
  TEST_ASSERT_EQUAL_UINT32(1200, ws_liveness::retransmitLadderMs(1));
  TEST_ASSERT_EQUAL_UINT32(3600, ws_liveness::retransmitLadderMs(2));
  TEST_ASSERT_EQUAL_UINT32(8400, ws_liveness::retransmitLadderMs(3));
  TEST_ASSERT_EQUAL_UINT32(18000, ws_liveness::retransmitLadderMs(4));
  TEST_ASSERT_EQUAL_UINT32(37200, ws_liveness::retransmitLadderMs(5));
  TEST_ASSERT_EQUAL_UINT32(75600, ws_liveness::retransmitLadderMs(6));
}

static void test_recovers_after_25s_hole() {
  // Session age 630 s (the pcap's first captured death), then a hole: no pong, no rx, lwIP
  // retransmitting. The old reaper killed this at 10 s; we must keep it through 25 s.
  for (uint32_t t = 0; t <= 25000; t += 1000) {
    Result r = ws_liveness::judge(client(630000 + t, t, /*retransmitting=*/true));
    TEST_ASSERT_TRUE_MESSAGE(r.verdict == Verdict::Keep, "evicted inside a 25 s hole");
  }
  // The hole clears: the queued pong lands, silence resets, back to "ok".
  Result r = ws_liveness::judge(client(656000, 200, false));
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  TEST_ASSERT_EQUAL_STRING("ok", r.reason);
}

static void test_evicts_after_45s_silence() {
  Result r = ws_liveness::judge(client(600000, 44999));
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  TEST_ASSERT_EQUAL_STRING("ok", r.reason);
  r = ws_liveness::judge(client(600000, 45000));
  TEST_ASSERT_TRUE(r.verdict == Verdict::EvictSilent);
  TEST_ASSERT_EQUAL_STRING("silent", r.reason);
  TEST_ASSERT_EQUAL_UINT32(45000, r.silence_ms);
  // ...and that is after HA's own 40 s window, so HA has already sent its close.
  TEST_ASSERT_TRUE(ws_liveness::LIVENESS_MS > ws_liveness::HA_PONG_TIMEOUT_MS);
}

static void test_retransmitting_extends_then_hard_caps() {
  Result r = ws_liveness::judge(client(600000, 45000, true));
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  TEST_ASSERT_EQUAL_STRING("extended", r.reason);
  r = ws_liveness::judge(client(600000, 119999, true));
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  TEST_ASSERT_EQUAL_STRING("extended", r.reason);
  r = ws_liveness::judge(client(600000, 120000, true));
  TEST_ASSERT_TRUE(r.verdict == Verdict::EvictSilent);
  TEST_ASSERT_EQUAL_STRING("silent-hard", r.reason);
}

static void test_ha_pings_keep_a_stuck_pong_socket_alive() {
  // Our pongs are stuck behind a hole (pong_age 90 s) but HA's pings still arrive (rx 8 s).
  Input in = client(600000, 0);
  in.pong_age_ms = 90000;
  in.rx_age_ms = 8000;
  Result r = ws_liveness::judge(in);
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  TEST_ASSERT_EQUAL_UINT32(8000, r.silence_ms);
}

static void test_young_socket_grace() {
  // Whatever the other inputs claim, a socket under MIN_AGE_MS is not judged.
  Result r = ws_liveness::judge(client(9999, 9999, false, 9999));
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  TEST_ASSERT_EQUAL_STRING("young", r.reason);
  Input odd = client(5000, 60000, false, 60000);  // inconsistent on purpose
  r = ws_liveness::judge(odd);
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  TEST_ASSERT_EQUAL_STRING("young", r.reason);
}

static void test_unwritable_reaps_at_30s_even_while_retransmitting() {
  Result r = ws_liveness::judge(client(20000, 5000, true, 29999));
  TEST_ASSERT_TRUE(r.verdict == Verdict::Keep);
  r = ws_liveness::judge(client(20000, 5000, true, 30000));
  TEST_ASSERT_TRUE(r.verdict == Verdict::EvictStalled);
  TEST_ASSERT_EQUAL_STRING("unwritable", r.reason);
  // Loop protection outranks the silence rule: unwritable AND silent-but-extended still reaps.
  r = ws_liveness::judge(client(600000, 50000, true, 30000));
  TEST_ASSERT_TRUE(r.verdict == Verdict::EvictStalled);
}

static void test_ping_is_gated_by_interval_writability_and_retransmit() {
  TEST_ASSERT_FALSE(ws_liveness::pingDue(14999, true, false));
  TEST_ASSERT_TRUE(ws_liveness::pingDue(15000, true, false));
  TEST_ASSERT_FALSE(ws_liveness::pingDue(15000, false, false));
  TEST_ASSERT_FALSE(ws_liveness::pingDue(15000, true, true));
  TEST_ASSERT_TRUE(ws_liveness::mayQueueTelemetry(true, false));
  TEST_ASSERT_FALSE(ws_liveness::mayQueueTelemetry(false, false));
  TEST_ASSERT_FALSE(ws_liveness::mayQueueTelemetry(true, true));
}

int main(int, char**) {
  UNITY_BEGIN();
  RUN_TEST(test_ladder_arithmetic_matches_the_capture);
  RUN_TEST(test_recovers_after_25s_hole);
  RUN_TEST(test_evicts_after_45s_silence);
  RUN_TEST(test_retransmitting_extends_then_hard_caps);
  RUN_TEST(test_ha_pings_keep_a_stuck_pong_socket_alive);
  RUN_TEST(test_young_socket_grace);
  RUN_TEST(test_unwritable_reaps_at_30s_even_while_retransmitting);
  RUN_TEST(test_ping_is_gated_by_interval_writability_and_retransmit);
  return UNITY_END();
}
