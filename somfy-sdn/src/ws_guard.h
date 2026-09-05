// A WebSocketsServer that refuses to hand a blocked socket to the library, emits its own
// keepalive pings through the same guard, and judges client liveness on the policy in
// ws_liveness.h rather than the library's pong reaper.
//
// Twin of actron-sniffer/src/ws_guard.{h,cpp} — keep the two in step.
//
// WHY THIS EXISTS (ledger shq-suite-0038, second mechanism).
//
// `WEBSOCKETS_TCP_TIMEOUT=500` fixed the *multi-operation* stalls, but a residual ~50 s
// main-loop stall survived on four independent devices with a 4 ms spread — one deterministic
// timeout, not an accumulation. It is in the Arduino core, not in lwIP and not in the WS
// library: `NetworkClient::write()` retries up to `WIFI_CLIENT_MAX_WRITE_RETRY` (10) times
// around a `select()` bounded by `WIFI_CLIENT_SELECT_TIMEOUT_US` (1 s), so ONE write to a peer
// that has stopped reading blocks the caller for ~10 s.
//
// Nothing above it can shorten that:
//   * `WEBSOCKETS_TCP_TIMEOUT` is only re-checked BETWEEN `write()` calls, never during one;
//   * `SO_SNDTIMEO` is irrelevant — the core's `send()` already uses `MSG_DONTWAIT`, so the
//     blocking is in the `select()`, not the send;
//   * both core constants are UNGUARDED `#define`s, so no `-D` build flag can override them,
//     and patching the framework would be a machine-global change that any toolchain update
//     silently reverts.
//
// `broadcastTXT()` walks every slot, and each frame costs a header write plus a payload write,
// so N stalled slots multiply that 10 s quantum into the 50-60 s stalls measured on the fleet
// (the pre-fix 60,069 ms and post-fix 50,05x ms figures are both clean multiples of 10 s — the
// earlier reading of them as multiples of the 5 s library timeout was a misattribution).
//
// The fix therefore lives here, in our own code: poll each socket for writability with a
// ZERO-timeout `select()` before writing, skip the ones that would block, and eventually drop
// a socket that stays unwritable long enough to be a zombie rather than merely busy. Broadcast
// cost becomes O(writable clients) and is bounded by the loop, not by a dead peer.
//
// WHAT CHANGED IN fw 1.14.0 (ledger shq-suite-0046). The library's `enableHeartbeat()` is no
// longer used at all. Its ping was written to the socket UNGUARDED from inside `loop()` — the
// one write this class could not intercept, and the sole reason the stall grace had to sit at
// 3 s (under the 15 s ping interval) — and its pong reaper evicted a client 10 s after the first
// missed pong, well inside lwIP's retransmit ladder, which is how a 10 s uplink fade became a
// dead session. Now: `sendPings()` emits the keepalive through the writability poll, and
// `judge()` applies ws_liveness::judge() per slot — silence in BOTH directions for 45 s (HA's
// own pings count as life), extended while lwIP is retransmitting, hard-capped at 120 s, plus
// the unwritable reap at 30 s. The derivation of every number is in ws_liveness.h.

#pragma once

#include <Arduino.h>
#include <WebSocketsServer.h>

#include <cstdint>

#include "ws_liveness.h"

class GuardedWebSocketsServer : public WebSocketsServer {
 public:
  using WebSocketsServer::WebSocketsServer;

  // True if this slot holds a client past the handshake.
  bool connected(uint8_t num);

  // True if this slot's socket would accept a write right now. Never blocks.
  bool writable(uint8_t num);

  // True if lwIP's last 1 Hz sample of this slot's pcb shows unacked bytes AND a retransmitted
  // head — the peer may be alive behind a hole (ws_liveness.h). False when no sample exists.
  bool retransmitting(uint8_t num);

  // Send to every connected client that is currently writable, skipping those that would
  // block. Returns the number skipped. This replaces broadcastTXT() everywhere.
  uint8_t broadcastWritableTXT(String& payload);

  // Single-client send with the same guard. Returns false (and counts a skip) if the socket
  // would block. Used for the per-client paths — the connect-time snapshot and the replies.
  bool sendWritableTXT(uint8_t num, String& payload);

  // Non-essential frames (diag records, health) additionally stay off a retransmitting pcb:
  // data queued behind a hole is coalesced by lwIP into the tail segment, i.e. it makes the
  // very frame that is failing longer. Returns false (counted as deferred) when held back.
  bool sendTelemetryTXT(uint8_t num, String& payload);
  uint8_t broadcastTelemetryTXT(String& payload);

  // Emit our keepalive ping on every slot where ws_liveness::pingDue() says so. Call once per
  // loop, after the library's loop() so a pong that just arrived is already counted.
  void sendPings(uint32_t now_ms);

  // Apply the liveness policy to every slot and drop the ones it evicts. Returns the number
  // dropped. Call once per loop, BEFORE the library's loop() and before any broadcast.
  uint8_t judge(uint32_t now_ms);

  // Lifetime counters — surfaced in the health push so the policy is measurable.
  uint32_t skippedWrites() const { return skipped_; }
  uint32_t reapedClients() const { return reaped_; }          // unwritable reaps
  uint32_t livenessEvicts() const { return evicts_; }         // silent evictions
  uint32_t livenessExtensions() const { return extended_; }   // silent-but-retransmitting holds
  // Judgements declined because the client was still inside MIN_AGE_MS while unwritable. A
  // climbing value means young sockets ARE going unwritable — the gate is doing real work.
  uint32_t deferredReaps() const { return deferred_; }
  uint32_t deferredTelemetry() const { return deferred_telemetry_; }  // held off a hole
  uint32_t pingSkips() const { return ping_skips_; }                  // pings held back
  uint32_t pingsSent() const { return pings_; }
  // Frames over ws_liveness::FRAME_BUDGET_BYTES that were sent anyway. Must stay at 0: the
  // whole hot path is sized under the budget, so a climb here is a regression.
  uint32_t bigFrames() const { return big_frames_; }

  // How long this slot has been unwritable, 0 if writable/idle. Evidence for a reap record.
  uint32_t unwritableForMs(uint8_t num, uint32_t now_ms) const;

  // The socket fd behind a connected slot, -1 otherwise. Read-only: used by the 1 Hz lwIP pcb
  // sampling (tcpsnap) so a disconnect record can say what TCP thought of the connection.
  int fdOf(uint8_t num);

 private:
  bool socketWritable(WSclient_t* c);
  void countFrame(const String& payload);

  // mono::now() when the slot first refused a write; 0 = writable or unoccupied.
  uint32_t unwritable_since_[WEBSOCKETS_SERVER_CLIENT_MAX] = {0};
  // mono::now() when the slot was first seen connected; 0 = unoccupied.
  uint32_t connected_since_[WEBSOCKETS_SERVER_CLIENT_MAX] = {0};
  // mono::now() when we last pinged the slot.
  uint32_t last_ping_[WEBSOCKETS_SERVER_CLIENT_MAX] = {0};
  uint32_t skipped_ = 0;
  uint32_t reaped_ = 0;
  uint32_t evicts_ = 0;
  uint32_t extended_ = 0;
  uint32_t deferred_ = 0;
  uint32_t deferred_telemetry_ = 0;
  uint32_t ping_skips_ = 0;
  uint32_t pings_ = 0;
  uint32_t big_frames_ = 0;
};
