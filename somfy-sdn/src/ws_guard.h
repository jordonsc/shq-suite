// A WebSocketsServer that refuses to hand a blocked socket to the library.
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

#pragma once

#include <Arduino.h>
#include <WebSocketsServer.h>

#include <cstdint>

// A socket continuously unwritable for this long is a zombie, not a busy peer. Chosen well
// below HA's 30 s availability timeout (so the reconnect lands before HA gives up) and well
// above any transient full send-buffer on a LAN.
constexpr uint32_t WS_STALL_REAP_MS = 10000;

class GuardedWebSocketsServer : public WebSocketsServer {
 public:
  using WebSocketsServer::WebSocketsServer;

  // True if this slot's socket would accept a write right now. Never blocks.
  bool writable(uint8_t num);

  // Send to every connected client that is currently writable, skipping those that would
  // block. Returns the number skipped. This replaces broadcastTXT() everywhere.
  uint8_t broadcastWritableTXT(String& payload);

  // Drop clients whose socket has been continuously unwritable for >= grace_ms. Returns the
  // number reaped. Call once per loop, before any broadcast.
  uint8_t reapStalled(uint32_t now_ms, uint32_t grace_ms);

  // Lifetime counters — surfaced in /stats and the health push so the fix is measurable.
  uint32_t skippedWrites() const { return skipped_; }
  uint32_t reapedClients() const { return reaped_; }

  // How long this slot has been unwritable, 0 if writable/idle. Evidence for a reap record.
  uint32_t unwritableForMs(uint8_t num, uint32_t now_ms) const;

 private:
  bool socketWritable(WSclient_t* c);

  // mono::now() when the slot first refused a write; 0 = writable or unoccupied.
  uint32_t unwritable_since_[WEBSOCKETS_SERVER_CLIENT_MAX] = {0};
  uint32_t skipped_ = 0;
  uint32_t reaped_ = 0;
};
