#include "ws_liveness.h"

namespace ws_liveness {

Result judge(const Input& in) {
  const uint32_t silence = in.pong_age_ms < in.rx_age_ms ? in.pong_age_ms : in.rx_age_ms;

  // Too young to judge, whatever the other inputs say (fw 1.9.0's lesson kept as an invariant).
  if (in.age_ms < MIN_AGE_MS) return {Verdict::Keep, "young", silence};

  // Loop protection first: a socket that will not accept writes costs the main loop ~10 s per
  // attempted write, and that is true whether or not the peer is alive behind a hole. Above the
  // 4th retransmission (18 s) an unwritable socket has >= TCP_SNDLOWAT stuck, which the hot path
  // (every frame <= FRAME_BUDGET_BYTES) cannot produce in one hole.
  if (in.unwritable_ms >= STALL_REAP_MS) return {Verdict::EvictStalled, "unwritable", silence};

  if (silence >= LIVENESS_HARD_MS) return {Verdict::EvictSilent, "silent-hard", silence};
  if (silence >= LIVENESS_MS) {
    if (in.retransmitting) return {Verdict::Keep, "extended", silence};
    return {Verdict::EvictSilent, "silent", silence};
  }
  return {Verdict::Keep, "ok", silence};
}

bool pingDue(uint32_t since_last_ping_ms, bool writable, bool retransmitting) {
  if (since_last_ping_ms < PING_INTERVAL_MS) return false;
  return writable && !retransmitting;
}

bool mayQueueTelemetry(bool writable, bool retransmitting) {
  return writable && !retransmitting;
}

}  // namespace ws_liveness
