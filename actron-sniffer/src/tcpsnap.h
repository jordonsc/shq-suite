// A snapshot of one TCP connection's lwIP protocol control block: what the stack itself thinks
// of the socket the write-guard is about to judge.
//
// Twin of somfy-sdn/src/tcpsnap.{h,cpp} — keep the two in step.
//
// WHY (fw 1.12.0, alongside txstats). The write-guard reports a socket as "unwritable" from a
// zero-timeout select(); that is a boolean and says nothing about WHY the send buffer is not
// draining. The pcb says: how many times the head segment has been retransmitted (`nrtx`), the
// current RTO, the congestion window, the bytes in flight the peer has not acknowledged, and
// the connection state. A socket "unwritable from birth" with nrtx climbing and a full unacked
// window is an uplink that is not getting through; one with nrtx=0 and an empty window is
// something else entirely. Captured at reap time, sampled once a second for every connected
// client so a disconnect record carries the last picture, and included in the health push.
//
// Reads the pcb list under lwIP's core lock (LOCK_TCPIP_CORE, CONFIG_LWIP_TCPIP_CORE_LOCKING=y
// in the prebuilt libs). Read-only: nothing is modified and the lock is held for one list walk.

#pragma once

#include <cstdint>

namespace tcpsnap {

struct Snap {
  uint8_t valid = 0;        // 1 when a matching pcb was found
  uint8_t state = 0;        // enum tcp_state (4 = ESTABLISHED, 7 = CLOSE_WAIT, ...)
  uint8_t nrtx = 0;         // retransmissions of the oldest unacked segment so far
  uint8_t snd_queuelen = 0; // pbufs queued for sending
  uint8_t dupacks = 0;      // duplicate ACKs seen for the current lastack
  uint16_t rto_ms = 0;      // current retransmission timeout
  uint16_t cwnd = 0;        // congestion window, bytes
  uint16_t snd_buf = 0;     // send buffer space still available, bytes
  uint16_t snd_wnd = 0;     // peer's advertised receive window, bytes
  uint16_t unacked = 0;     // snd_nxt - lastack: bytes in flight
  uint16_t flags = 0;       // pcb->flags (TF_RTO 0x0800 = an RTO has fired on it)
};

// If the 16-byte IPv6 address `b` (network order) is an IPv4-MAPPED address (::ffff:a.b.c.d),
// store the embedded IPv4 in `*v4` as the 4 network-order bytes lwIP keeps in ip4_addr_t and
// return true. Pure, host-tested: this is the check fw 1.12.x lacked (see capture()).
bool mappedV4(const uint8_t b[16], uint32_t* v4);

#ifdef ARDUINO
// Look up the connection behind a socket fd (via getpeername) and fill `out`. Returns false
// (out.valid = 0) if the fd has no peer or no active pcb matches it.
//
// The Arduino NetworkServer listens on an AF_INET6 dual-stack socket (NetworkServer.cpp:
// socket(AF_INET6) bound to in6addr_any when LWIP_IPV6 is on, which it is in the prebuilt C6
// libs), so lwIP reports every IPv4 peer of an accepted socket as an IPv4-mapped IPv6 sockaddr
// (family AF_INET6, ::ffff:a.b.c.d) while the pcb itself stays v4-typed. fw 1.12.x rejected
// AF_INET6 here and so never matched a pcb — no tcp_* key ever reached a health push or a
// disconnect record. 1.13.0 unmaps it, the same way the core's NetworkClient::remoteIP() does.
bool capture(int fd, Snap& out);

// Lifetime capture() calls that had an fd but found no pcb — the instrument reporting its own
// failure, surfaced as `tcp_miss` in the health push. Should stay at 0 with a client connected.
uint32_t misses();
#endif

const char* stateName(uint8_t state);

}  // namespace tcpsnap
