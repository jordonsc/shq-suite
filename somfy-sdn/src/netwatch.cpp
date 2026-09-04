#include "netwatch.h"

namespace netwatch {

bool Policy::cooledDown(uint32_t now) const {
  return !ever_reassoc_ || (uint32_t)(now - last_reassoc_ms_) >= REASSOC_COOLDOWN_MS;
}

Verdict Policy::issue(Action a, const char* reason, uint32_t now) {
  last_reason_ = reason;
  if (a == Action::Reassociate) {
    ever_reassoc_ = true;
    last_reassoc_ms_ = now;
    recoveries_++;
  } else if (a == Action::Reboot) {
    reboots_++;
  }
  return Verdict{a, reason};
}

Verdict Policy::step(const Input& in) {
  const uint32_t now = in.now_ms;

  // Trigger 1 bookkeeping runs even with the link down: a step is a fact about the clock, and
  // it must not be lost just because the link happened to be flapping at that moment.
  if (!started_) {
    started_ = true;
    seen_rebases_ = in.rebases;  // history predating us is not ours to act on
  } else if (in.rebases != seen_rebases_) {
    seen_rebases_ = in.rebases;
    if (in.last_rebase_ms < 0 &&
        (uint32_t)(-(int64_t)in.last_rebase_ms) >= CLOCK_STEP_REASSOC_MS) {
      clock_pending_ = true;
    }
  }

  if (!in.link_up) {
    // wifi_prov's link watchdog owns a DOWN link (link-retry, then wifi-dead reboot). Nothing
    // here is meaningful without an association, so hold every outage timer at zero.
    fail_streak_ = 0;
    unreach_since_ = 0;
    unreach_acted_ = false;
    heap_low_since_ = 0;
    heap_acted_ = false;
    return Verdict{Action::None, last_reason_};
  }

  // Trigger 2 bookkeeping.
  const bool inbound_fresh = in.inbound_age_ms < INBOUND_FRESH_MS;
  if (in.probe == Probe::Ok || inbound_fresh) {
    if (in.probe == Probe::Ok) fail_streak_ = 0;
    unreach_since_ = 0;
    unreach_acted_ = false;
  } else if (in.probe == Probe::Fail) {
    fail_streak_++;
    if (unreach_since_ == 0) unreach_since_ = now;
  }

  // Trigger 3 bookkeeping, with hysteresis.
  if (in.heap < HEAP_LOW_BYTES) {
    if (heap_low_since_ == 0) heap_low_since_ = now;
  } else if (in.heap > HEAP_CLEAR_BYTES) {
    heap_low_since_ = 0;
    heap_acted_ = false;
  }

  // Decisions, most urgent first. Reboots ignore the cooldown; re-associations honour it.

  if (unreach_acted_ && unreach_since_ != 0 &&
      (uint32_t)(now - unreach_acted_at_) >= UNREACH_REBOOT_MS) {
    if (allow_reboot_) return issue(Action::Reboot, "stack-dead", now);
    if (cooledDown(now)) {
      unreach_acted_at_ = now;
      return issue(Action::Reassociate, "unreachable", now);
    }
  }

  if (heap_acted_ && heap_low_since_ != 0 &&
      (uint32_t)(now - heap_acted_at_) >= HEAP_REBOOT_MS) {
    if (allow_reboot_) return issue(Action::Reboot, "heap-low", now);
    if (cooledDown(now)) {
      heap_acted_at_ = now;
      return issue(Action::Reassociate, "heap-low", now);
    }
  }

  if (clock_pending_ && cooledDown(now)) {
    clock_pending_ = false;
    return issue(Action::Reassociate, "clock-step", now);
  }

  if (!unreach_acted_ && unreach_since_ != 0 &&
      (uint32_t)(now - unreach_since_) >= UNREACH_REASSOC_MS && cooledDown(now)) {
    unreach_acted_ = true;
    unreach_acted_at_ = now;
    return issue(Action::Reassociate, "unreachable", now);
  }

  if (!heap_acted_ && heap_low_since_ != 0 &&
      (uint32_t)(now - heap_low_since_) >= HEAP_REASSOC_MS && cooledDown(now)) {
    heap_acted_ = true;
    heap_acted_at_ = now;
    return issue(Action::Reassociate, "heap-low", now);
  }

  return Verdict{Action::None, last_reason_};
}

}  // namespace netwatch
