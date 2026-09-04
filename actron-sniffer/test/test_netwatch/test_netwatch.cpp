// Host tests for the network-stack watchdog policy (fw 1.11.0, ledger shq-suite-0044).
//
// The properties that matter, each taken from the Bed 2 outage it is meant to prevent:
//   * a large backward clock step re-associates at once, a small one does nothing;
//   * "unreachable" means the gateway AND the WS peer have both gone quiet, so an HA outage on
//     its own never trips it;
//   * every trigger re-associates before it reboots, and the reboot tier can be switched off.

#include <string.h>
#include <unity.h>

#include "netwatch.h"

using netwatch::Action;
using netwatch::Input;
using netwatch::Policy;
using netwatch::Probe;
using netwatch::Verdict;

void setUp() {}
void tearDown() {}

static const uint32_t NEVER = 0xFFFFFFFFu;
static const uint32_t HEAP_OK = 230000u;

static Input healthy(uint32_t now) {
  Input in{};
  in.now_ms = now;
  in.link_up = true;
  in.probe = Probe::None;
  in.rebases = 0;
  in.last_rebase_ms = 0;
  in.heap = HEAP_OK;
  in.inbound_age_ms = NEVER;
  return in;
}

// Run `p` forward one tick per second from `from` to `to` (exclusive) with inputs from `mk`,
// returning the first non-None verdict, or None with the final time.
template <typename F>
static Verdict runUntilAction(Policy& p, uint32_t from, uint32_t to, F mk, uint32_t* at) {
  for (uint32_t t = from; t < to; t += 1000) {
    Verdict v = p.step(mk(t));
    if (v.action != Action::None) {
      if (at) *at = t;
      return v;
    }
  }
  if (at) *at = to;
  return Verdict{Action::None, ""};
}

void test_quiet_device_never_acts() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, [](uint32_t t) {
    Input in = healthy(t);
    if (t % 60000 == 0) in.probe = Probe::Ok;
    return in;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::None);
  TEST_ASSERT_EQUAL_UINT32(0, p.recoveries());
  TEST_ASSERT_EQUAL_STRING("none", p.lastReason());
}

// ---- trigger 1: clock step -------------------------------------------------

void test_large_backward_step_reassociates_immediately() {
  Policy p;
  p.step(healthy(1000));
  Input in = healthy(2000);
  in.rebases = 1;
  in.last_rebase_ms = -150323756;  // Bed 2's 35 x 2^32 us step
  Verdict v = p.step(in);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  TEST_ASSERT_EQUAL_STRING("clock-step", v.reason);
  TEST_ASSERT_EQUAL_UINT32(1, p.recoveries());
}

void test_small_backward_step_is_ignored() {
  Policy p;
  p.step(healthy(1000));
  Input in = healthy(2000);
  in.rebases = 1;
  in.last_rebase_ms = -216;  // Bed 4's step
  TEST_ASSERT_TRUE(p.step(in).action == Action::None);
}

void test_forward_rebase_is_ignored() {
  Policy p;
  p.step(healthy(1000));
  Input in = healthy(2000);
  in.rebases = 1;
  in.last_rebase_ms = 150323756;
  TEST_ASSERT_TRUE(p.step(in).action == Action::None);
}

void test_rebases_predating_boot_of_policy_are_not_acted_on() {
  Policy p;
  Input in = healthy(1000);
  in.rebases = 2;
  in.last_rebase_ms = -150323756;
  TEST_ASSERT_TRUE(p.step(in).action == Action::None);  // first sample sets the baseline
  TEST_ASSERT_TRUE(p.step(in).action == Action::None);  // unchanged count: nothing new
}

void test_clock_step_never_reboots_and_honours_cooldown() {
  Policy p;
  p.step(healthy(1000));
  Input in = healthy(2000);
  in.rebases = 1;
  in.last_rebase_ms = -1111383;
  TEST_ASSERT_TRUE(p.step(in).action == Action::Reassociate);
  // A second step inside the cooldown is held, then released once the cooldown lapses.
  in.now_ms = 3000;
  in.rebases = 2;
  TEST_ASSERT_TRUE(p.step(in).action == Action::None);
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 4000, 10u * 60u * 1000u, [&](uint32_t t) {
    Input i = in;
    i.now_ms = t;
    return i;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  TEST_ASSERT_EQUAL_STRING("clock-step", v.reason);
  TEST_ASSERT_TRUE(at >= 2000 + netwatch::REASSOC_COOLDOWN_MS);
  TEST_ASSERT_EQUAL_UINT32(0, p.reboots());
}

// ---- trigger 2: unreachable -------------------------------------------------

static Input failingProbes(uint32_t t) {
  Input in = healthy(t);
  if (t % 60000 == 0) in.probe = Probe::Fail;
  return in;
}

void test_unreachable_reassociates_then_reboots() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, failingProbes, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  TEST_ASSERT_EQUAL_STRING("unreachable", v.reason);
  TEST_ASSERT_TRUE(at >= netwatch::UNREACH_REASSOC_MS);
  TEST_ASSERT_TRUE(at < netwatch::UNREACH_REASSOC_MS + 61000);
  TEST_ASSERT_TRUE(p.consecutiveFailures() >= 3);

  const uint32_t reassoc_at = at;
  v = runUntilAction(p, at + 1000, at + 60u * 60u * 1000u, failingProbes, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reboot);
  TEST_ASSERT_EQUAL_STRING("stack-dead", v.reason);
  TEST_ASSERT_TRUE(at >= reassoc_at + netwatch::UNREACH_REBOOT_MS);
  TEST_ASSERT_EQUAL_UINT32(1, p.recoveries());
  TEST_ASSERT_EQUAL_UINT32(1, p.reboots());
}

void test_probe_success_after_reassociation_cancels_reboot() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, failingProbes, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  // The re-association worked: probes answer again.
  v = runUntilAction(p, at + 1000, at + 60u * 60u * 1000u, [](uint32_t t) {
    Input in = healthy(t);
    if (t % 60000 == 0) in.probe = Probe::Ok;
    return in;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::None);
  TEST_ASSERT_FALSE(p.unreachable());
  TEST_ASSERT_EQUAL_UINT32(0, p.consecutiveFailures());
  TEST_ASSERT_EQUAL_UINT32(0, p.reboots());
}

// THE discriminator the user asked for: HA talking to us proves the stack works, even if the
// gateway has stopped answering ICMP. Nothing may fire.
void test_inbound_ws_traffic_overrides_failed_probes() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, [](uint32_t t) {
    Input in = failingProbes(t);
    in.inbound_age_ms = 15000;  // HA pongs every 15 s
    return in;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::None);
  TEST_ASSERT_FALSE(p.unreachable());
}

// ...and the converse: HA going away on its own (probes fine) is not our problem.
void test_ha_outage_alone_never_acts() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 6u * 60u * 60u * 1000u, [](uint32_t t) {
    Input in = healthy(t);
    if (t % 60000 == 0) in.probe = Probe::Ok;
    in.inbound_age_ms = NEVER;
    return in;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::None);
  TEST_ASSERT_EQUAL_UINT32(0, p.recoveries());
}

void test_link_down_is_left_to_the_link_watchdog() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, [](uint32_t t) {
    Input in = failingProbes(t);
    in.link_up = false;
    in.heap = 20000;
    return in;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::None);
}

void test_reboot_tier_can_be_disabled() {
  Policy p(false);
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, failingProbes, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  // Keep failing for a long time: only ever re-associations, spaced by the cooldown.
  uint32_t last = at;
  for (int i = 0; i < 6; i++) {
    v = runUntilAction(p, last + 1000, last + 60u * 60u * 1000u, failingProbes, &at);
    TEST_ASSERT_TRUE(v.action == Action::Reassociate);
    TEST_ASSERT_TRUE(at - last >= netwatch::REASSOC_COOLDOWN_MS);
    last = at;
  }
  TEST_ASSERT_EQUAL_UINT32(0, p.reboots());
  TEST_ASSERT_EQUAL_UINT32(7, p.recoveries());
}

// ---- trigger 3: heap ---------------------------------------------------------

static Input lowHeap(uint32_t t) {
  Input in = healthy(t);
  in.heap = 40000;
  if (t % 60000 == 0) in.probe = Probe::Ok;  // gateway is fine, HA is fine: heap alone
  in.inbound_age_ms = 5000;
  return in;
}

void test_heap_low_reassociates_then_reboots() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, lowHeap, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  TEST_ASSERT_EQUAL_STRING("heap-low", v.reason);
  TEST_ASSERT_TRUE(at >= netwatch::HEAP_REASSOC_MS);
  const uint32_t first = at;
  v = runUntilAction(p, at + 1000, at + 60u * 60u * 1000u, lowHeap, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reboot);
  TEST_ASSERT_EQUAL_STRING("heap-low", v.reason);
  TEST_ASSERT_TRUE(at >= first + netwatch::HEAP_REBOOT_MS);
}

void test_heap_recovery_after_reassociation_cancels_reboot() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, lowHeap, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  // The re-association freed the heap (this is what happened on Bed 2: 8 kB -> 240 kB).
  v = runUntilAction(p, at + 1000, at + 60u * 60u * 1000u, [](uint32_t t) {
    Input in = lowHeap(t);
    in.heap = HEAP_OK;
    return in;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::None);
  TEST_ASSERT_FALSE(p.heapLow());
  TEST_ASSERT_EQUAL_UINT32(0, p.reboots());
}

void test_heap_hysteresis_does_not_disarm_on_a_brief_bounce() {
  Policy p;
  uint32_t at = 0;
  // Heap hovers between LOW and CLEAR: still armed, so it eventually acts.
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, [](uint32_t t) {
    Input in = lowHeap(t);
    in.heap = (t / 1000) % 2 ? 40000 : 70000;
    return in;
  }, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  TEST_ASSERT_EQUAL_STRING("heap-low", v.reason);
}

// A reboot verdict must not be gated by the re-association cooldown: by then waiting is the
// thing that kills the device.
void test_reboot_ignores_cooldown() {
  Policy p;
  uint32_t at = 0;
  Verdict v = runUntilAction(p, 0, 60u * 60u * 1000u, failingProbes, &at);
  TEST_ASSERT_TRUE(v.action == Action::Reassociate);
  // A clock step lands right after and (once the cooldown lapses) re-associates again, which
  // pushes last_reassoc forward; the unreachable reboot must still fire on its own schedule.
  const uint32_t reassoc_at = at;
  v = runUntilAction(p, at + 1000, at + 60u * 60u * 1000u, [&](uint32_t t) {
    Input in = failingProbes(t);
    if (t == reassoc_at + 1000) {
      in.rebases = 1;
      in.last_rebase_ms = -100000;
    } else {
      in.rebases = 1;
      in.last_rebase_ms = -100000;
    }
    return in;
  }, &at);
  // First thing out is the reboot (due at +5 min) or the deferred clock re-association (due at
  // +5 min cooldown) — both land on the same tick; the reboot is evaluated first.
  TEST_ASSERT_TRUE(v.action == Action::Reboot);
  TEST_ASSERT_EQUAL_STRING("stack-dead", v.reason);
}

int main(int, char**) {
  UNITY_BEGIN();
  RUN_TEST(test_quiet_device_never_acts);
  RUN_TEST(test_large_backward_step_reassociates_immediately);
  RUN_TEST(test_small_backward_step_is_ignored);
  RUN_TEST(test_forward_rebase_is_ignored);
  RUN_TEST(test_rebases_predating_boot_of_policy_are_not_acted_on);
  RUN_TEST(test_clock_step_never_reboots_and_honours_cooldown);
  RUN_TEST(test_unreachable_reassociates_then_reboots);
  RUN_TEST(test_probe_success_after_reassociation_cancels_reboot);
  RUN_TEST(test_inbound_ws_traffic_overrides_failed_probes);
  RUN_TEST(test_ha_outage_alone_never_acts);
  RUN_TEST(test_link_down_is_left_to_the_link_watchdog);
  RUN_TEST(test_reboot_tier_can_be_disabled);
  RUN_TEST(test_heap_low_reassociates_then_reboots);
  RUN_TEST(test_heap_recovery_after_reassociation_cancels_reboot);
  RUN_TEST(test_heap_hysteresis_does_not_disarm_on_a_brief_bounce);
  RUN_TEST(test_reboot_ignores_cooldown);
  return UNITY_END();
}
