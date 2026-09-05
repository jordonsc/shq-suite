#include "ws_guard.h"

#include <sys/select.h>
#include <sys/time.h>

#include "diag.h"
#include "tcpsnap.h"

namespace {
// Never 0: the per-slot stamps use 0 as their "unset" sentinel.
uint32_t stamp(uint32_t now_ms) { return now_ms == 0 ? 1u : now_ms; }
}  // namespace

int GuardedWebSocketsServer::fdOf(uint8_t num) {
  if (num >= WEBSOCKETS_SERVER_CLIENT_MAX) return -1;
  WSclient_t* c = &_clients[num];
  if (c->status != WSC_CONNECTED || c->tcp == nullptr || !c->tcp->connected()) return -1;
  return c->tcp->fd();
}

bool GuardedWebSocketsServer::socketWritable(WSclient_t* c) {
  if (c == nullptr || c->tcp == nullptr) return false;
  if (!c->tcp->connected()) return false;
  const int fd = c->tcp->fd();
  if (fd < 0) return false;

  fd_set w;
  FD_ZERO(&w);
  FD_SET(fd, &w);
  // Zero timeout: a poll, never a wait. This is the same readiness test
  // NetworkClient::write() runs internally, minus the 1 s it is willing to block for.
  // lwIP reports writable only while more than TCP_SNDLOWAT (2873 B here) of send buffer is
  // free and fewer than TCP_SNDQUEUELOWAT (8) pbufs are queued — so "unwritable" means at
  // least ~2.9 kB is stuck unacknowledged, not merely that the peer is slow.
  struct timeval tv = {0, 0};
  const int r = ::select(fd + 1, nullptr, &w, nullptr, &tv);
  return r > 0 && FD_ISSET(fd, &w);
}

bool GuardedWebSocketsServer::connected(uint8_t num) {
  if (num >= WEBSOCKETS_SERVER_CLIENT_MAX) return false;
  return _clients[num].status == WSC_CONNECTED;
}

bool GuardedWebSocketsServer::writable(uint8_t num) {
  if (!connected(num)) return false;
  return socketWritable(&_clients[num]);
}

bool GuardedWebSocketsServer::retransmitting(uint8_t num) {
  const tcpsnap::Snap* t = diag::lastTcp(num);
  return t != nullptr && t->valid && t->nrtx > 0 && t->unacked > 0;
}

uint32_t GuardedWebSocketsServer::unwritableForMs(uint8_t num, uint32_t now_ms) const {
  if (num >= WEBSOCKETS_SERVER_CLIENT_MAX) return 0;
  const uint32_t since = unwritable_since_[num];
  if (since == 0) return 0;
  return (uint32_t)(now_ms - since);
}

void GuardedWebSocketsServer::countFrame(const String& payload) {
  if (payload.length() > ws_liveness::FRAME_BUDGET_BYTES) big_frames_++;
}

uint8_t GuardedWebSocketsServer::broadcastWritableTXT(String& payload) {
  uint8_t skipped = 0;
  countFrame(payload);
  for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
    WSclient_t* c = &_clients[i];
    if (c->status != WSC_CONNECTED) continue;
    if (!socketWritable(c)) {
      // Writing here would cost ~10 s inside the core's retry loop. Skip it: the slot is
      // either about to drain (next broadcast catches up — every payload is a full snapshot,
      // never a delta, so a skipped frame loses nothing) or it is dead and judge() will
      // drop it.
      skipped++;
      skipped_++;
      continue;
    }
    sendTXT(i, payload);
  }
  return skipped;
}

bool GuardedWebSocketsServer::sendWritableTXT(uint8_t num, String& payload) {
  if (!connected(num)) return false;
  countFrame(payload);
  if (!socketWritable(&_clients[num])) {
    skipped_++;
    return false;
  }
  sendTXT(num, payload);
  return true;
}

bool GuardedWebSocketsServer::sendTelemetryTXT(uint8_t num, String& payload) {
  if (!connected(num)) return false;
  if (!ws_liveness::mayQueueTelemetry(socketWritable(&_clients[num]), retransmitting(num))) {
    deferred_telemetry_++;
    return false;
  }
  countFrame(payload);
  sendTXT(num, payload);
  return true;
}

uint8_t GuardedWebSocketsServer::broadcastTelemetryTXT(String& payload) {
  uint8_t held = 0;
  for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
    if (_clients[i].status != WSC_CONNECTED) continue;
    if (!sendTelemetryTXT(i, payload)) held++;
  }
  return held;
}

void GuardedWebSocketsServer::sendPings(uint32_t now_ms) {
  for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
    WSclient_t* c = &_clients[i];
    if (c->status != WSC_CONNECTED) continue;
    if (last_ping_[i] == 0) last_ping_[i] = stamp(now_ms);  // first sighting: ping in 15 s
    const uint32_t since = (uint32_t)(now_ms - last_ping_[i]);
    if (since < ws_liveness::PING_INTERVAL_MS) continue;
    if (!ws_liveness::pingDue(since, socketWritable(c), retransmitting(i))) {
      ping_skips_++;
      continue;
    }
    if (sendPing(i)) {
      pings_++;
      last_ping_[i] = stamp(now_ms);
    }
  }
}

uint8_t GuardedWebSocketsServer::judge(uint32_t now_ms) {
  uint8_t dropped = 0;
  for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
    WSclient_t* c = &_clients[i];
    if (c->status != WSC_CONNECTED) {
      unwritable_since_[i] = 0;
      connected_since_[i] = 0;
      last_ping_[i] = 0;
      continue;
    }
    // First sighting of this slot as connected — start its age clock.
    if (connected_since_[i] == 0) connected_since_[i] = stamp(now_ms);

    const bool writable_now = socketWritable(c);
    if (writable_now) {
      unwritable_since_[i] = 0;
    } else if (unwritable_since_[i] == 0) {
      unwritable_since_[i] = stamp(now_ms);
    }

    ws_liveness::Input in{};
    in.age_ms = (uint32_t)(now_ms - connected_since_[i]);
    in.unwritable_ms = writable_now ? 0 : (uint32_t)(now_ms - unwritable_since_[i]);
    in.retransmitting = retransmitting(i);
    // diag owns the per-slot pong/rx stamps (it already keeps them for the disconnect
    // classifier); a slot diag does not know about reads as "as old as the socket".
    if (!diag::slotAges(i, now_ms, &in.pong_age_ms, &in.rx_age_ms)) {
      in.pong_age_ms = in.age_ms;
      in.rx_age_ms = in.age_ms;
    }

    const ws_liveness::Result r = ws_liveness::judge(in);
    if (r.verdict == ws_liveness::Verdict::Keep) {
      if (!writable_now && r.reason[0] == 'y') deferred_++;   // "young"
      if (r.reason[0] == 'e') extended_++;                    // "extended"
      continue;
    }

    unwritable_since_[i] = 0;
    connected_since_[i] = 0;
    last_ping_[i] = 0;
    dropped++;
    // What lwIP thinks of the socket at the instant we give up on it — retransmit count,
    // RTO, bytes in flight — captured BEFORE the pcb is torn down (fw 1.12.0).
    tcpsnap::Snap snap;
    tcpsnap::capture(fdOf(i), snap);
    if (r.verdict == ws_liveness::Verdict::EvictStalled) {
      reaped_++;
      diag::noteWsStallReap(i, in.unwritable_ms, connectedClients(), &snap);
      // Close the socket directly. Sending a WS close frame would be a write to the very
      // socket that is refusing writes — i.e. the 10 s stall this class exists to avoid. The
      // library notices !connected() on its next pass and runs its normal disconnect path, so
      // WStype_DISCONNECTED (and the diag classification hanging off it) still fires.
      if (c->tcp != nullptr) c->tcp->stop();
    } else {
      evicts_++;
      diag::noteWsEvict(i, r.reason, r.silence_ms, connectedClients(), &snap);
      // A silent peer whose socket still accepts writes gets a proper close frame (1000) so a
      // client that is merely deaf learns why; one that would block is stopped the same way
      // as a stall reap.
      if (writable_now) disconnect(i);
      else if (c->tcp != nullptr) c->tcp->stop();
    }
  }
  return dropped;
}
