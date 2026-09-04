// Network-stack watchdog policy (fw 1.11.0, ledger shq-suite-0044).
//
// Twin of actron-sniffer/src/netwatch.h — keep the two in step (same rule as mono/fault/diag).
//
// WHY. On 2026-09-02/03 the Bed 2 controller's clock stepped BACKWARDS by 35 x 2^32 us. The
// mono re-baseline (fw 1.10.0) kept every deadline in THIS firmware alive, but esp_timer's
// absolute alarms live on the same hardware counter and nothing re-arms them: the WiFi driver
// beneath us was timer-dead for the size of the step, leaked ~3.3 B/s, and 18 h later the
// receive path starved. The station stayed ASSOCIATED the whole time — it kept transmitting a
// gratuitous ARP every 60 s — while answering nothing: no ARP reply, no ICMP, no TCP, no mDNS.
// The router said "healthy client"; HA said "unreachable"; both were right. Neither reboot
// backstop fired because both key on the STA link being DOWN. A router-side reconnect cured it
// in one second, no reboot: a re-association tears the driver's timers down and re-arms them
// against the current counter.
//
// WHAT. Three independent triggers, each with the same two-tier response — re-associate first
// (cheap, ~5 s, keeps the RAM diagnostic ring), reboot only if that did not help:
//
//   1. CLOCK STEP.  An adopted backward re-baseline of CLOCK_STEP_REASSOC_MS or more means the
//      IDF's timers are now blacked out for that long. Re-associate immediately; do not wait for
//      the symptom. Never reboots — the step itself is handled, this only refreshes the driver.
//   2. UNREACHABLE. The gateway stops answering probes while the link claims to be up, and
//      nothing inbound has arrived over WS either — literally "nobody can talk to me". An HA
//      outage cannot trip this: the gateway still answers. Re-associate after
//      UNREACH_REASSOC_MS; reboot if still unreachable UNREACH_REBOOT_MS after that.
//   3. HEAP LOW.    Free heap under HEAP_LOW_BYTES for HEAP_REASSOC_MS — whatever the cause, the
//      receive path dies around 15 kB, so act while there is room to. Re-associate; reboot if
//      still low HEAP_REBOOT_MS later.
//
// A single re-association cooldown stops the three triggers from thrashing the link. The reboot
// tier can be disabled wholesale (`allow_reboot=false`): the actron twin sits on a physically cut
// RS485 bus and must never reboot with the A/C running (shq-suite-0042), so there the second
// tier repeats the re-association instead.
//
// Pure and Arduino-free so the policy is host-testable (`pio test -e native`, test_netwatch).
// The glue that probes the gateway and performs the actions lives in wifi_prov.cpp.

#pragma once

#include <cstdint>

namespace netwatch {

// Trigger 1: adopted backward clock step at least this large re-associates. Below a minute the
// timer blackout is shorter than the leak needs to matter.
constexpr uint32_t CLOCK_STEP_REASSOC_MS = 60u * 1000u;

// Trigger 2: gateway unreachable (and no inbound WS traffic) continuously this long.
constexpr uint32_t UNREACH_REASSOC_MS = 3u * 60u * 1000u;
// ...and still unreachable this long AFTER the re-association => reboot.
constexpr uint32_t UNREACH_REBOOT_MS = 5u * 60u * 1000u;

// Inbound WS traffic (a frame or a pong) younger than this proves the stack can receive, which
// overrides a failed probe. This is what makes the trigger mean "nobody can reach me" rather
// than "the gateway dropped ICMP".
constexpr uint32_t INBOUND_FRESH_MS = 60u * 1000u;

// Trigger 3: heap thresholds. LOW arms the timer; CLEAR (hysteresis) disarms it. Matches the
// heap_low fault in main.cpp; the receive path dies at ~15 kB, so 60 kB leaves room to act.
constexpr uint32_t HEAP_LOW_BYTES = 60000u;
constexpr uint32_t HEAP_CLEAR_BYTES = 80000u;
constexpr uint32_t HEAP_REASSOC_MS = 10u * 60u * 1000u;
constexpr uint32_t HEAP_REBOOT_MS = 10u * 60u * 1000u;

// Minimum spacing between re-associations, whatever the trigger.
constexpr uint32_t REASSOC_COOLDOWN_MS = 5u * 60u * 1000u;

enum class Action : uint8_t { None = 0, Reassociate, Reboot };

// Result of the most recent gateway probe, or None when no probe completed this tick.
enum class Probe : uint8_t { None = 0, Ok, Fail };

struct Input {
  uint32_t now_ms;          // mono::now()
  bool link_up;             // WiFi.isConnected()
  Probe probe;              // newest completed probe, None if nothing new
  uint32_t rebases;         // mono::rebases()
  int32_t last_rebase_ms;   // mono::lastRebaseMs(), signed
  uint32_t heap;            // ESP.getFreeHeap()
  uint32_t inbound_age_ms;  // now - last inbound WS frame/pong; UINT32_MAX if never
};

struct Verdict {
  Action action;
  const char* reason;  // static string: "clock-step", "unreachable", "stack-dead", "heap-low"
};

class Policy {
 public:
  explicit Policy(bool allow_reboot = true) : allow_reboot_(allow_reboot) {}

  // Feed once per tick (~1 Hz). Returns at most one action; the caller performs it.
  Verdict step(const Input& in);

  // Telemetry.
  uint32_t consecutiveFailures() const { return fail_streak_; }  // probes unanswered in a row
  uint32_t recoveries() const { return recoveries_; }            // re-associations issued
  uint32_t reboots() const { return reboots_; }                  // reboot verdicts issued
  const char* lastReason() const { return last_reason_; }        // "none" until the first action
  bool unreachable() const { return unreach_since_ != 0; }
  bool heapLow() const { return heap_low_since_ != 0; }

 private:
  Verdict issue(Action a, const char* reason, uint32_t now);
  bool cooledDown(uint32_t now) const;

  bool allow_reboot_;

  // Trigger 1.
  bool started_ = false;
  uint32_t seen_rebases_ = 0;
  bool clock_pending_ = false;

  // Trigger 2.
  uint32_t fail_streak_ = 0;
  uint32_t unreach_since_ = 0;        // 0 = reachable
  bool unreach_acted_ = false;        // re-associated for this outage already
  uint32_t unreach_acted_at_ = 0;

  // Trigger 3.
  uint32_t heap_low_since_ = 0;       // 0 = heap fine
  bool heap_acted_ = false;
  uint32_t heap_acted_at_ = 0;

  // Shared.
  bool ever_reassoc_ = false;
  uint32_t last_reassoc_ms_ = 0;
  uint32_t recoveries_ = 0;
  uint32_t reboots_ = 0;
  const char* last_reason_ = "none";
};

}  // namespace netwatch
