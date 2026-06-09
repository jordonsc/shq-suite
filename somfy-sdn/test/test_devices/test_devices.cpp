// Host-side unit tests for the device table. Run with: pio test -e native
//
// Validates: upsert/find + source priority; online transition via touch; position application
// (change detection, fault on sentinel, movement completion, stall detection); limits
// application; comms-loss sweep; table-full behaviour.

#include <unity.h>

#include <cstdint>
#include <cstring>

#include "devices.h"
#include "sdn.h"

using namespace devices;

void setUp() {}
void tearDown() {}

static const uint8_t A1[3] = {0x23, 0x00, 0x01};
static const uint8_t A2[3] = {0x51, 0x00, 0x01};

static sdn::PositionReport pos(uint8_t pct, uint16_t pulses = 100) {
  sdn::PositionReport r;
  r.valid = true;
  r.fault = false;
  r.percent = pct;
  r.pulses = pulses;
  return r;
}

void test_upsert_find_and_source_priority() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 1000);
  TEST_ASSERT_NOT_NULL(d);
  TEST_ASSERT_EQUAL(1, t.count());
  TEST_ASSERT_EQUAL_PTR(d, t.find(A1));
  TEST_ASSERT_NULL(t.find(A2));

  // Re-upsert with a more authoritative source upgrades it; same slot.
  Device* d2 = t.upsert(A1, Source::CONFIGURED, 2000);
  TEST_ASSERT_EQUAL_PTR(d, d2);
  TEST_ASSERT_EQUAL(1, t.count());
  TEST_ASSERT_EQUAL((int)Source::CONFIGURED, (int)d->source);

  // A less authoritative source must NOT downgrade.
  t.upsert(A1, Source::OBSERVED, 3000);
  TEST_ASSERT_EQUAL((int)Source::CONFIGURED, (int)d->source);
}

void test_touch_online_transition() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 1000);
  TEST_ASSERT_FALSE(d->online);
  bool became = false;
  t.touch(d, 1500, &became);
  TEST_ASSERT_TRUE(d->online);
  TEST_ASSERT_TRUE(became);
  t.touch(d, 1600, &became);
  TEST_ASSERT_FALSE(became);  // already online
}

void test_apply_position_change_and_completion() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 0);

  PosResult r1 = t.applyPosition(d, pos(0), 100);  // first reading (open)
  TEST_ASSERT_TRUE(r1.changed);
  TEST_ASSERT_TRUE(d->position_known);
  TEST_ASSERT_EQUAL_UINT8(0, d->position_pct);

  // Command a move down to 100, then feed intermediate + final positions.
  t.beginMove(d, sdn::MovementState::MOVING_DOWN, 100);
  TEST_ASSERT_EQUAL((int)sdn::MovementState::MOVING_DOWN, (int)d->movement);

  PosResult mid = t.applyPosition(d, pos(50), 200);
  TEST_ASSERT_TRUE(mid.changed);
  TEST_ASSERT_FALSE(mid.movement_complete);
  TEST_ASSERT_EQUAL((int)sdn::MovementState::MOVING_DOWN, (int)d->movement);

  PosResult done = t.applyPosition(d, pos(100), 300);
  TEST_ASSERT_TRUE(done.movement_complete);
  TEST_ASSERT_EQUAL((int)sdn::MovementState::IDLE, (int)d->movement);
}

// An idle motor reporting a changed percent while its encoder pulse count is unchanged is a
// glitch (the field-observed percent=100/pulses-frozen flap) — it must be suppressed, and a real
// move (pulses change) must still apply.
void test_apply_position_ignores_spurious_percent_when_idle() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 0);

  t.applyPosition(d, pos(32, 446), 100);  // settled: 32% at 446 pulses
  TEST_ASSERT_EQUAL_UINT8(32, d->position_pct);

  // Idle + pulses unchanged but percent jumps to 100 → spurious, suppressed (no change broadcast).
  PosResult glitch = t.applyPosition(d, pos(100, 446), 200);
  TEST_ASSERT_FALSE(glitch.changed);
  TEST_ASSERT_EQUAL_UINT8(32, d->position_pct);

  // A genuine move (pulses advance) still applies even if idle (e.g. manual pull / external move).
  PosResult real = t.applyPosition(d, pos(60, 800), 300);
  TEST_ASSERT_TRUE(real.changed);
  TEST_ASSERT_EQUAL_UINT8(60, d->position_pct);
}

void test_apply_position_fault_and_clear() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 0);
  t.applyPosition(d, pos(20), 100);

  sdn::PositionReport fault;
  fault.valid = false;
  fault.fault = true;
  PosResult r = t.applyPosition(d, fault, 200);
  TEST_ASSERT_TRUE(r.became_fault);
  TEST_ASSERT_TRUE(d->fault);

  PosResult r2 = t.applyPosition(d, pos(20), 300);
  TEST_ASSERT_TRUE(r2.cleared_fault);
  TEST_ASSERT_FALSE(d->fault);
}

void test_stall_detection() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 0);
  t.applyPosition(d, pos(30), 0);
  t.beginMove(d, sdn::MovementState::MOVING_UP, 0);

  // Feed the same position repeatedly. Break the instant the stall trips and check state at
  // that point — a later valid read would clear the fault again (app_sdn model: any valid
  // position clears a prior fault), so the fault is only guaranteed sticky right after the trip.
  bool stalled = false;
  for (int i = 0; i < STALL_THRESHOLD + 5; i++) {
    PosResult r = t.applyPosition(d, pos(30), (uint32_t)(100 + i));  // never moves
    if (r.stalled) {
      stalled = true;
      TEST_ASSERT_TRUE(d->fault);
      TEST_ASSERT_EQUAL((int)sdn::MovementState::IDLE, (int)d->movement);
      break;
    }
  }
  TEST_ASSERT_TRUE(stalled);
}

void test_apply_limits() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 0);
  sdn::LimitsReport lr;
  lr.valid = true;
  lr.up_pulses = 16;
  lr.down_pulses = 900;
  TEST_ASSERT_TRUE(t.applyLimits(d, lr));      // first set => changed
  TEST_ASSERT_TRUE(d->limits_known);
  TEST_ASSERT_EQUAL_UINT16(900, d->down_limit_pulses);
  TEST_ASSERT_FALSE(t.applyLimits(d, lr));     // same => no change

  // 0xFFFF on both limits = unset (factory / after reset) => limits_known clears.
  sdn::LimitsReport unset;
  unset.valid = true;
  unset.up_pulses = 0xFFFF;
  unset.down_pulses = 0xFFFF;
  TEST_ASSERT_TRUE(t.applyLimits(d, unset));
  TEST_ASSERT_FALSE(d->limits_known);
}

void test_movement_clears_on_fault() {
  DeviceTable t;
  Device* d = t.upsert(A1, Source::OBSERVED, 0);
  t.applyPosition(d, pos(30), 0);
  t.beginMove(d, sdn::MovementState::MOVING_DOWN, 100);
  // A faulted reading mid-move must drop movement to IDLE (not leave it stuck).
  sdn::PositionReport fault;
  fault.valid = false;
  fault.fault = true;
  t.applyPosition(d, fault, 100);
  TEST_ASSERT_EQUAL((int)sdn::MovementState::IDLE, (int)d->movement);
  // ...and again while already faulted (the bug we fixed: stays IDLE, not re-stuck).
  t.beginMove(d, sdn::MovementState::MOVING_UP, 0);
  t.applyPosition(d, fault, 200);
  TEST_ASSERT_EQUAL((int)sdn::MovementState::IDLE, (int)d->movement);
}

void test_sweep_offline() {
  DeviceTable t;
  Device* d1 = t.upsert(A1, Source::OBSERVED, 0);
  Device* d2 = t.upsert(A2, Source::OBSERVED, 0);
  t.touch(d1, 1000, nullptr);
  t.touch(d2, 1000, nullptr);

  Device* offline[MAX_DEVICES];
  // 1000 + 89s: nobody offline yet.
  size_t n = t.sweepOffline(1000 + 89000, DEFAULT_OFFLINE_MS, offline, MAX_DEVICES);
  TEST_ASSERT_EQUAL(0, n);

  // Refresh d2; advance past the offline window — only d1 should drop.
  t.touch(d2, 1000 + 89000, nullptr);
  n = t.sweepOffline(1000 + 91000, DEFAULT_OFFLINE_MS, offline, MAX_DEVICES);
  TEST_ASSERT_EQUAL(1, n);
  TEST_ASSERT_EQUAL_PTR(d1, offline[0]);
  TEST_ASSERT_FALSE(d1->online);
  TEST_ASSERT_TRUE(d2->online);
}

void test_table_full() {
  DeviceTable t;
  for (size_t i = 0; i < MAX_DEVICES; i++) {
    uint8_t a[3] = {(uint8_t)(i + 1), 0x00, 0x01};
    TEST_ASSERT_NOT_NULL(t.upsert(a, Source::OBSERVED, 0));
  }
  TEST_ASSERT_EQUAL(MAX_DEVICES, t.count());
  uint8_t overflow[3] = {0xEE, 0xEE, 0xEE};
  TEST_ASSERT_NULL(t.upsert(overflow, Source::OBSERVED, 0));
}

int main(int /*argc*/, char** /*argv*/) {
  UNITY_BEGIN();
  RUN_TEST(test_upsert_find_and_source_priority);
  RUN_TEST(test_touch_online_transition);
  RUN_TEST(test_apply_position_change_and_completion);
  RUN_TEST(test_apply_position_ignores_spurious_percent_when_idle);
  RUN_TEST(test_apply_position_fault_and_clear);
  RUN_TEST(test_stall_detection);
  RUN_TEST(test_apply_limits);
  RUN_TEST(test_movement_clears_on_fault);
  RUN_TEST(test_sweep_offline);
  RUN_TEST(test_table_full);
  return UNITY_END();
}
