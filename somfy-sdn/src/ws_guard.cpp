#include "ws_guard.h"

#include <sys/select.h>
#include <sys/time.h>

#include "diag.h"

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
  struct timeval tv = {0, 0};
  const int r = ::select(fd + 1, nullptr, &w, nullptr, &tv);
  return r > 0 && FD_ISSET(fd, &w);
}

bool GuardedWebSocketsServer::writable(uint8_t num) {
  if (num >= WEBSOCKETS_SERVER_CLIENT_MAX) return false;
  WSclient_t* c = &_clients[num];
  if (c->status != WSC_CONNECTED) return false;
  return socketWritable(c);
}

uint32_t GuardedWebSocketsServer::unwritableForMs(uint8_t num, uint32_t now_ms) const {
  if (num >= WEBSOCKETS_SERVER_CLIENT_MAX) return 0;
  const uint32_t since = unwritable_since_[num];
  if (since == 0) return 0;
  return (uint32_t)(now_ms - since);
}

uint8_t GuardedWebSocketsServer::broadcastWritableTXT(String& payload) {
  uint8_t skipped = 0;
  for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
    WSclient_t* c = &_clients[i];
    if (c->status != WSC_CONNECTED) continue;
    if (!socketWritable(c)) {
      // Writing here would cost ~10 s inside the core's retry loop. Skip it: the slot is
      // either about to drain (next broadcast catches up — every payload is a full snapshot,
      // never a delta, so a skipped frame loses nothing) or it is dead and reapStalled()
      // will drop it.
      skipped++;
      skipped_++;
      continue;
    }
    sendTXT(i, payload);
  }
  return skipped;
}

uint8_t GuardedWebSocketsServer::reapStalled(uint32_t now_ms, uint32_t grace_ms) {
  uint8_t reaped = 0;
  for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
    WSclient_t* c = &_clients[i];
    if (c->status != WSC_CONNECTED) {
      unwritable_since_[i] = 0;
      continue;
    }
    if (socketWritable(c)) {
      unwritable_since_[i] = 0;
      continue;
    }
    if (unwritable_since_[i] == 0) {
      unwritable_since_[i] = (now_ms == 0) ? 1u : now_ms;  // 0 is the "writable" sentinel
      continue;
    }
    const uint32_t stuck_ms = (uint32_t)(now_ms - unwritable_since_[i]);
    if (stuck_ms < grace_ms) continue;

    unwritable_since_[i] = 0;
    reaped++;
    reaped_++;
    diag::noteWsStallReap(i, stuck_ms, connectedClients());
    // Close the socket directly. Sending a WS close frame would be a write to the very
    // socket that is refusing writes — i.e. the 10 s stall this class exists to avoid. The
    // library notices !connected() on its next pass and runs its normal disconnect path, so
    // WStype_DISCONNECTED (and the diag classification hanging off it) still fires.
    if (c->tcp != nullptr) c->tcp->stop();
  }
  return reaped;
}
