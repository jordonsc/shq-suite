#include "tcpsnap.h"

#include <cstring>

namespace tcpsnap {

const char* stateName(uint8_t s) {
  switch (s) {
    case 0: return "closed";
    case 1: return "listen";
    case 2: return "syn_sent";
    case 3: return "syn_rcvd";
    case 4: return "established";
    case 5: return "fin_wait_1";
    case 6: return "fin_wait_2";
    case 7: return "close_wait";
    case 8: return "closing";
    case 9: return "last_ack";
    case 10: return "time_wait";
  }
  return "?";
}

bool mappedV4(const uint8_t b[16], uint32_t* v4) {
  if (b == nullptr || v4 == nullptr) return false;
  for (int i = 0; i < 10; i++) {
    if (b[i] != 0) return false;
  }
  if (b[10] != 0xFF || b[11] != 0xFF) return false;
  uint32_t out;
  memcpy(&out, b + 12, 4);  // keep network byte order: that is what ip4_addr_get_u32() yields
  *v4 = out;
  return true;
}

}  // namespace tcpsnap

#ifdef ARDUINO

#include <lwip/sockets.h>
#include <lwip/tcpip.h>
#include <lwip/priv/tcp_priv.h>

#include <cstring>

namespace tcpsnap {

namespace {

template <typename T>
uint16_t clamp16(T v) { return v > 0xFFFF ? 0xFFFF : (uint16_t)v; }

uint32_t g_misses = 0;

}  // namespace

uint32_t misses() { return g_misses; }

bool capture(int fd, Snap& out) {
  out = Snap{};
  if (fd < 0) return false;
  struct sockaddr_storage ss;
  socklen_t len = sizeof(ss);
  if (lwip_getpeername(fd, (struct sockaddr*)&ss, &len) != 0) return false;
  uint32_t peer_ip = 0;  // network order, as lwIP stores it in the pcb
  uint16_t peer_port = 0;
  if (ss.ss_family == AF_INET) {
    const struct sockaddr_in* sin = (const struct sockaddr_in*)&ss;
    peer_ip = sin->sin_addr.s_addr;
    peer_port = lwip_ntohs(sin->sin_port);
  } else if (ss.ss_family == AF_INET6) {
    // Dual-stack listener (see tcpsnap.h): an IPv4 peer arrives here as ::ffff:a.b.c.d. A real
    // IPv6 peer is not something this LAN produces and is not matched.
    const struct sockaddr_in6* sin6 = (const struct sockaddr_in6*)&ss;
    if (!mappedV4(sin6->sin6_addr.un.u8_addr, &peer_ip)) return false;
    peer_port = lwip_ntohs(sin6->sin6_port);
  } else {
    return false;
  }

  LOCK_TCPIP_CORE();
  for (const struct tcp_pcb* pcb = tcp_active_pcbs; pcb != nullptr; pcb = pcb->next) {
    if (!IP_IS_V4_VAL(pcb->remote_ip)) continue;
    if (pcb->remote_port != peer_port) continue;
    if (ip4_addr_get_u32(ip_2_ip4(&pcb->remote_ip)) != peer_ip) continue;
    out.valid = 1;
    out.state = (uint8_t)pcb->state;
    out.nrtx = pcb->nrtx;
    out.snd_queuelen = (uint8_t)(pcb->snd_queuelen > 0xFF ? 0xFF : pcb->snd_queuelen);
    out.dupacks = pcb->dupacks;
    out.rto_ms = clamp16((uint32_t)pcb->rto * TCP_SLOW_INTERVAL);
    out.cwnd = clamp16(pcb->cwnd);
    out.snd_buf = clamp16(pcb->snd_buf);
    out.snd_wnd = clamp16(pcb->snd_wnd);
    out.unacked = clamp16((uint32_t)(pcb->snd_nxt - pcb->lastack));
    out.flags = pcb->flags;
    break;
  }
  UNLOCK_TCPIP_CORE();
  if (!out.valid) g_misses++;
  return out.valid != 0;
}

}  // namespace tcpsnap

#endif  // ARDUINO
