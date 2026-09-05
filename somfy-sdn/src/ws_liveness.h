// WebSocket client liveness policy (fw 1.14.0, ledger shq-suite-0046).
//
// Twin of actron-sniffer/src/ws_liveness.{h,cpp} — keep the two in step (same rule as
// mono/fault/diag/netwatch/ws_guard). Pure and Arduino-free so every rule is host-testable
// (`pio test -e native`, test_ws_liveness); the glue that feeds it lives in ws_guard.cpp.
//
// WHY A PING/PONG CANNOT MEASURE LIVENESS ON A RETRANSMITTING TCP FLOW.
//
// The pcap that settled shq-suite-0046 showed the fatal sequence on the wire. On two boards
// uplink WiFi frames of ~700 B and up are lost in bursts of 7-30 s while small frames get
// through. TCP delivers in order, so ONE lost segment blocks everything queued behind it: lwIP
// retransmits from the head with exponential backoff (measured 1.2, 2.4, 4.8, 9.6, 19.2 s ...)
// and ignores the peer's SACK, so the 2-byte ping we wrote after the lost frame, the 6-byte pong
// we owe HA, and the 330-byte state push all reach HA's kernel and sit in its out-of-order queue
// where the application cannot see them. Meanwhile HA's own ping reaches us fine (downlink is
// clean) and our pong to it is stuck behind the same hole. So during a hole:
//
//   * our pong never arrives at HA's application, whatever HA does;
//   * HA's pong to OUR ping never arrives here — HA has not even seen the ping;
//   * HA's pings DO arrive here, and we DO ACK them at the TCP layer.
//
// A pong is therefore evidence of a working uplink, not of a living peer, and its absence says
// "the head segment has not landed yet" — a statement about the retransmit ladder, not about HA.
// The library's `enableHeartbeat(15000, 5000, 2)` did not know this: it re-pinged immediately
// after a missed pong (`handleHBTimeout` sets `lastPing` back) and evicted 10 s after the first
// unanswered ping, i.e. after only three retransmissions (+1.2, +3.6, +8.4 s) of a segment lwIP
// would have kept retrying for minutes. 42 recorded sessions died with pong_age = 25.0 s exactly.
//
// WHAT THIS DOES INSTEAD. The library never evicts (its heartbeat is not enabled). We emit the
// ping ourselves through the write-guard, and judge a client from three facts:
//
//   1. SILENCE = time since the last inbound frame of ANY kind (pong, ping, text). HA pings us
//      every 20 s and a hole does not stop those arriving, so a live HA behind a hole keeps this
//      fresh. Only a peer that is genuinely gone — or a socket dead in both directions — is
//      silent.
//   2. RETRANSMITTING = lwIP has unacked bytes on the pcb and has retransmitted the head at least
//      once (`tcpsnap` nrtx > 0 && unacked > 0). The peer may well be alive behind a hole, so a
//      silent-but-retransmitting socket gets more time, up to a hard cap.
//   3. UNWRITABLE = `select()` says a write would block: TCP_SNDLOWAT (2873 B on this build,
//      min(max(SND_BUF/2, 2*MSS+1), SND_BUF-1)) of send buffer is gone. This is the write-guard's
//      own signal (shq-suite-0038) and still reaps, but on its own deadline.
//
// THE NUMBERS, each derived from the ladder and from HA's own timers:
//
//   PING_INTERVAL_MS   15 s   unchanged; two of our pings per HA availability window.
//   LIVENESS_MS        45 s   above HA's 40 s (20 s ping + 20 s pong timeout) so HA gives up
//                             first and sends a close frame that a clean downlink delivers — and
//                             above the 5th retransmission (1.2+2.4+4.8+9.6+19.2 = 37.2 s).
//   LIVENESS_HARD_MS  120 s   evict a silent socket even while lwIP is still retransmitting:
//                             above the 6th retransmission (75.6 s), and by then HA has torn the
//                             session down itself twice over. A peer that vanished mid-hole
//                             cannot hold a slot for lwIP's full MAXRTX (12) ladder (~minutes).
//   STALL_REAP_MS      30 s   unwritable this long => reap. Above the 4th retransmission (18 s).
//                             The old 3 s existed only because the LIBRARY wrote its ping to
//                             the socket unguarded inside loop() and a blocked slot cost a ~10 s
//                             core write. With no library heartbeat and our ping routed through
//                             the guard, the only unguarded writes left are the library's own
//                             replies inside loop() — a PONG to a peer PING (HA pings every
//                             20 s) and a close frame to a peer CLOSE — so an unwritable slot
//                             can still cost ~10 s per HA ping until it is reaped. Bounded, and
//                             reachable only once > TCP_SNDLOWAT is stuck; kept in mind.
//   MIN_AGE_MS         10 s   a client younger than this is never judged. Now implied by every
//                             deadline above, kept as an explicit invariant (fw 1.9.0's lesson:
//                             a coordinator mid-handshake looks dead at the socket layer).
//   FRAME_BUDGET_BYTES 600    the largest frame this firmware may send on the hot path. Loss
//                             rose steeply with length on the affected boards: 0/313 at ~330 B,
//                             8/65 at ~720 B, 2/4 at >=1300 B. A diag record with the tcp_* keys
//                             is ~554 B; the health push is split into three frames under this.
//
// The base RTO of 1.2 s below is what the capture measured on these sockets (RTT-derived);
// CONFIG_LWIP_TCP_RTO_TIME's 3 s is only the pre-RTT initial value.

#pragma once

#include <cstdint>

namespace ws_liveness {

constexpr uint32_t PING_INTERVAL_MS = 15000;
constexpr uint32_t LIVENESS_MS = 45000;
constexpr uint32_t LIVENESS_HARD_MS = 120000;
constexpr uint32_t STALL_REAP_MS = 30000;
constexpr uint32_t MIN_AGE_MS = 10000;
constexpr uint16_t FRAME_BUDGET_BYTES = 600;

// HA's own liveness window: websockets ping_interval=20 + ping_timeout=20 (client.py).
constexpr uint32_t HA_PONG_TIMEOUT_MS = 40000;

// Cumulative time to the n-th retransmission of the head segment on lwIP's doubling ladder,
// from the measured 1.2 s base RTO: 1.2, 3.6, 8.4, 18.0, 37.2, 75.6 s ...
constexpr uint32_t RTO_BASE_MS = 1200;
constexpr uint32_t retransmitLadderMs(uint8_t n) {
  return n == 0 ? 0u : retransmitLadderMs(n - 1) + (RTO_BASE_MS << (n - 1));
}

static_assert(LIVENESS_MS > HA_PONG_TIMEOUT_MS, "HA must give up on a hole before we do");
static_assert(LIVENESS_MS > retransmitLadderMs(5), "liveness must outlast five retransmissions");
static_assert(LIVENESS_HARD_MS > retransmitLadderMs(6), "hard cap must outlast six retransmissions");
static_assert(LIVENESS_HARD_MS > LIVENESS_MS, "hard cap is the extension, not the rule");
static_assert(STALL_REAP_MS > retransmitLadderMs(4), "stall reap must outlast four retransmissions");
static_assert(STALL_REAP_MS >= MIN_AGE_MS, "min age is implied by the stall deadline");
static_assert(PING_INTERVAL_MS * 2 <= LIVENESS_MS, "at least two of our pings per liveness window");

enum class Verdict : uint8_t { Keep = 0, EvictSilent, EvictStalled };

struct Input {
  uint32_t age_ms;         // since the handshake completed
  uint32_t pong_age_ms;    // since the last pong (== age_ms if none yet)
  uint32_t rx_age_ms;      // since the last inbound frame of any kind (== age_ms if none yet)
  uint32_t unwritable_ms;  // continuously unwritable; 0 = writable right now
  bool retransmitting;     // pcb has unacked bytes AND nrtx > 0
};

struct Result {
  Verdict verdict;
  const char* reason;   // static: "ok" "young" "extended" "silent" "silent-hard" "unwritable"
  uint32_t silence_ms;  // min(pong_age, rx_age) — the evidence a record carries
};

// Judge one client. Pure: same input, same verdict.
Result judge(const Input& in);

// Is our periodic ping due, and may it go out? Never while the socket would block (that is the
// ~10 s core write the guard exists to avoid) and never while lwIP is retransmitting: a ping
// queued behind a hole only lengthens the tail segment, and the pong it would earn is already
// known to be stuck.
bool pingDue(uint32_t since_last_ping_ms, bool writable, bool retransmitting);

// May a non-essential frame (a diag record, a health push) be queued now? Same two conditions.
// State pushes are NOT gated by this — they are what HA's availability rides on and they are
// under the frame budget; they still skip an unwritable socket via the guard.
bool mayQueueTelemetry(bool writable, bool retransmitting);

}  // namespace ws_liveness
