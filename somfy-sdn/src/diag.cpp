#include "diag.h"

#include <Arduino.h>
#include <WiFi.h>
#include <esp_system.h>
#include <lwip/sockets.h>

#include <cstdio>
#include <cstring>

#include "mono.h"

namespace diag {

namespace {

// More slots than WEBSOCKETS_SERVER_CLIENT_MAX (5) so a client id from a future library bump
// can't run off the end. Every index is bounds-checked anyway.
constexpr uint8_t MAX_SLOTS = 8;

// How often tick() re-probes the socket pool. The probe itself is two syscalls, but it briefly
// holds sockets it is measuring the scarcity of — so don't run it every loop.
constexpr uint32_t SOCKET_PROBE_INTERVAL_MS = 15000;

struct Slot {
  bool active = false;
  uint32_t connected_ms = 0;
  uint32_t last_pong_ms = 0;
  uint32_t last_rx_ms = 0;
  uint32_t last_error_ms = 0;
  uint16_t rx_msgs = 0;
  uint16_t tx_msgs = 0;
  char ip[16] = {0};
};

Slot slots_[MAX_SLOTS];

Record ring_[RING_CAPACITY];
size_t ring_count_ = 0;    // records ever written, capped conceptually — see firstSeq()
uint32_t next_seq_ = 1;

// Phase maxima. `*_max_` is since boot (a permanent worst-case for /stats); `*_win_` resets every
// time a record is written, so each record reports the worst stall in the run-up TO it — which is
// the number that matters when asking "did the loop stall right before this eviction?".
uint32_t loop_max_ = 0, loop_win_ = 0;
uint32_t http_max_ = 0, http_win_ = 0;
uint32_t ota_max_ = 0, ws_max_ = 0;
uint32_t loop_stalls_ = 0;
uint32_t loop_iters_ = 0;
uint32_t loop_busy_ms_ = 0;  // summed iteration time, for a coarse duty figure

uint32_t pong_timeouts_ = 0;
uint32_t stall_reaps_ = 0;
uint32_t wifi_roams_ = 0;
char bssid_[18] = {'-', 0};
uint32_t peer_closes_ = 0;
uint32_t transport_errors_ = 0;
uint32_t wifi_disc_ = 0;

uint8_t spare_sockets_ = SOCKET_PROBE_MAX;
uint32_t last_socket_probe_ms_ = 0;
bool socket_low_latched_ = false;
bool heap_low_latched_ = false;
bool hb_stall_latched_ = false;
uint32_t last_clock_glitches_ = 0;
bool wifi_up_ = true;

// DELIBERATELY EXCLUDES backwardReads() (ledger shq-suite-0039). `mono::Filter` is documented as
// not internally synchronised, and somfy-sdn calls mono::now() from tight deadline-poll loops in
// its bus task concurrently with the Arduino loop — which produced 25.4 MILLION "backward" reads on
// Bed 4 over four days (~72/s) with a perfectly healthy clock. Including that here would fire a
// ClockGlitch record every single tick and flood the ring with noise. torn and jump are the two
// counters that actually fingerprint the shq-suite-0034 far-future-millis glitch; back is a race
// artifact. (On actron this is inert — bridgeTask never calls mono::now(), so back stays 0.)
uint32_t clockGlitchTotal() {
  return mono::tornReads() + mono::forwardJumps();
}

// Ask lwIP for sockets until it says no. The count we get back is the headroom left in a pool
// that HTTP, WS, OTA and mDNS all draw from — the single number that settles whether the socket
// layer is starved (ledger shq-suite-0038, hypothesis b).
uint8_t probeSockets() {
  int fds[SOCKET_PROBE_MAX];
  uint8_t n = 0;
  while (n < SOCKET_PROBE_MAX) {
    int fd = lwip_socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) break;
    fds[n++] = fd;
  }
  for (uint8_t i = 0; i < n; i++) lwip_close(fds[i]);
  return n;
}

Record& push(Event ev) {
  Record& r = ring_[ring_count_ % RING_CAPACITY];
  memset(&r, 0, sizeof(r));
  r.seq = next_seq_++;
  r.t_ms = mono::now();
  r.ev = ev;
  r.reason = Reason::None;
  r.heap = ESP.getFreeHeap();
  r.max_block = ESP.getMaxAllocHeap();
  r.spare_sockets = spare_sockets_;
  r.rssi = WiFi.isConnected() ? (int8_t)WiFi.RSSI() : 0;
  r.loop_max_ms = loop_win_;
  r.http_max_ms = http_win_;
  loop_win_ = 0;
  http_win_ = 0;
  ring_count_++;
  return r;
}

}  // namespace

// ---- lifecycle ---------------------------------------------------------

void begin() {
  spare_sockets_ = probeSockets();
  last_socket_probe_ms_ = mono::now();
  last_clock_glitches_ = clockGlitchTotal();
  wifi_up_ = WiFi.isConnected();
  Record& r = push(Event::Boot);
  r.value = (uint32_t)esp_reset_reason();
}

void noteLoop(uint32_t total_ms, uint32_t ota_ms, uint32_t http_ms, uint32_t ws_ms) {
  loop_iters_++;
  loop_busy_ms_ += total_ms;
  if (total_ms > loop_max_) loop_max_ = total_ms;
  if (total_ms > loop_win_) loop_win_ = total_ms;
  if (http_ms > http_max_) http_max_ = http_ms;
  if (http_ms > http_win_) http_win_ = http_ms;
  if (ota_ms > ota_max_) ota_max_ = ota_ms;
  if (ws_ms > ws_max_) ws_max_ = ws_ms;

  if (total_ms >= LOOP_STALL_MS) {
    loop_stalls_++;
    // Record the attribution, not just the duration: `value` is the whole iteration, and the
    // three phases go into the fields a disconnect record would otherwise use. Whichever phase
    // is closest to `value` is the one that blocked the WS pump.
    Record& r = push(Event::LoopStall);
    r.value = total_ms;
    r.lifetime_ms = ota_ms;
    r.pong_age_ms = http_ms;
    r.rx_msgs = (uint16_t)(ws_ms > 0xFFFF ? 0xFFFF : ws_ms);
  }
}

void tick(uint32_t hb_age_ms) {
  // Wrap guard (ledger shq-suite-0038): an hb_age in the top half of uint32 range is a wrapped
  // "negative" — a heartbeat stamp read as sitting in the future — not a 24-day stall. On this
  // firmware the heartbeat stamp has always been single-writer (main loop), but the actron twin
  // proved a nonsense value can latch a bogus heartbeat_stall record; keep the twins in step.
  if (hb_age_ms >= 0x80000000u) hb_age_ms = 0;
  const uint32_t now = mono::now();

  // Called from every loop iteration (thousands per second), but everything below is a sampling
  // watch, not an event handler. ESP.getFreeHeap() takes a heap lock — running it at loop rate
  // would add contention to the very subsystem under investigation.
  static uint32_t last_tick_ms = 0;
  if ((uint32_t)(now - last_tick_ms) < 1000) return;
  last_tick_ms = now;

  if ((uint32_t)(now - last_socket_probe_ms_) >= SOCKET_PROBE_INTERVAL_MS) {
    last_socket_probe_ms_ = now;
    spare_sockets_ = probeSockets();
    if (spare_sockets_ <= SOCKET_LOW_SPARE) {
      if (!socket_low_latched_) {
        socket_low_latched_ = true;
        Record& r = push(Event::SocketLow);
        r.value = spare_sockets_;
      }
    } else {
      socket_low_latched_ = false;
    }
  }

  const uint32_t heap = ESP.getFreeHeap();
  if (!heap_low_latched_ && heap < HEAP_LOW_BYTES) {
    heap_low_latched_ = true;
    Record& r = push(Event::HeapLow);
    r.value = heap;
  } else if (heap_low_latched_ && heap > HEAP_LOW_CLEAR_BYTES) {
    heap_low_latched_ = false;
  }

  // The clock filter firing at all is news — it never has on this unit (clk_* were flat zero
  // across the whole shq-suite-0038 investigation), so a single glitch changes the diagnosis.
  const uint32_t glitches = clockGlitchTotal();
  if (glitches != last_clock_glitches_) {
    Record& r = push(Event::ClockGlitch);
    r.value = glitches;
    r.lifetime_ms = mono::tornReads();
    r.pong_age_ms = mono::forwardJumps();
    r.rx_msgs = (uint16_t)(mono::backwardReads() > 0xFFFF ? 0xFFFF : mono::backwardReads());
    last_clock_glitches_ = glitches;
  }

  // The shq-suite-0034 signature, watched for directly rather than inferred from /stats later.
  if (!hb_stall_latched_ && hb_age_ms >= HEARTBEAT_STALL_MS) {
    hb_stall_latched_ = true;
    Record& r = push(Event::HeartbeatStall);
    r.value = hb_age_ms;
  } else if (hb_stall_latched_ && hb_age_ms < HEARTBEAT_STALL_MS / 2) {
    hb_stall_latched_ = false;
  }

  // Which AP is serving us, watched from the STATION side (ledger shq-suite-0038 / argus-0117).
  // A flap that follows one BSSID rather than one device is an AP fault, not a firmware fault —
  // and that distinction is unreachable without this, since the firmware previously never
  // reported its association at all.
  if (WiFi.isConnected()) {
    const String b = WiFi.BSSIDstr();
    if (b.length() > 0 && b != bssid_) {
      const bool first = (bssid_[0] == '-' && bssid_[1] == 0);
      if (!first) {
        wifi_roams_++;
        Record& r = push(Event::ApChange);
        r.value = wifi_roams_;
        snprintf(r.ip, sizeof(r.ip), "%s", b.c_str() + 6);  // last 3 octets identify the radio
        Serial.printf("[diag] AP change: %s -> %s\n", bssid_, b.c_str());
      }
      snprintf(bssid_, sizeof(bssid_), "%s", b.c_str());
    }
  }

  const bool up = WiFi.isConnected();
  if (up != wifi_up_) {
    wifi_up_ = up;
    if (!up) wifi_disc_++;
    Record& r = push(up ? Event::WifiUp : Event::WifiDown);
    r.value = up ? (uint32_t)(int32_t)WiFi.RSSI() : wifi_disc_;
  }
}

// ---- WS lifecycle hooks ------------------------------------------------

void noteWsConnect(uint8_t client_id, const char* ip, uint8_t clients) {
  const uint32_t now = mono::now();
  if (client_id < MAX_SLOTS) {
    Slot& s = slots_[client_id];
    s = Slot{};
    s.active = true;
    s.connected_ms = now;
    s.last_pong_ms = now;  // a fresh handshake counts as proof of life
    s.last_rx_ms = now;
    if (ip != nullptr) strncpy(s.ip, ip, sizeof(s.ip) - 1);
  }
  Record& r = push(Event::WsConnect);
  r.client_id = client_id;
  r.clients = clients;
  if (ip != nullptr) strncpy(r.ip, ip, sizeof(r.ip) - 1);

  // Worth its own record: at the cap the library stops answering handshakes entirely, which is
  // indistinguishable from a dead device at the far end.
  if (clients >= 5) {
    Record& cap = push(Event::WsAtCap);
    cap.clients = clients;
    cap.value = clients;
  }
}

void noteWsDisconnect(uint8_t client_id, uint8_t clients) {
  const uint32_t now = mono::now();
  // Re-probe rather than reuse the periodic sample: whether the pool was empty AT THIS INSTANT is
  // the whole question, and a value up to 15 s stale would not answer it.
  spare_sockets_ = probeSockets();

  Reason reason = Reason::Unknown;
  uint32_t lifetime = 0, pong_age = 0;
  uint16_t rx = 0, tx = 0;
  char ip[16] = {0};

  if (client_id < MAX_SLOTS && slots_[client_id].active) {
    Slot& s = slots_[client_id];
    lifetime = (uint32_t)(now - s.connected_ms);
    pong_age = (uint32_t)(now - s.last_pong_ms);
    rx = s.rx_msgs;
    tx = s.tx_msgs;
    strncpy(ip, s.ip, sizeof(ip) - 1);

    if (s.last_error_ms != 0 && (uint32_t)(now - s.last_error_ms) <= 2000) {
      reason = Reason::TransportError;
      transport_errors_++;
    } else if (pong_age >= PONG_OVERDUE_MS) {
      // Our own reaper. The client may be in perfect health — a main-loop stall long enough to
      // miss the pong window produces exactly this, which is why loop_max_ms rides along.
      reason = Reason::PongTimeout;
      pong_timeouts_++;
    } else if (pong_age <= WS_PING_INTERVAL_MS ||
               (uint32_t)(now - s.last_rx_ms) <= PEER_ACTIVE_MS) {
      // A pong inside the last ping cycle rules our reaper out entirely — it only evicts after
      // consecutive misses — so whatever closed this socket, it was not us. The library gives no
      // callback for a received close frame, which is why this is inferred rather than observed:
      // HA's own `ha_clean`/`ha_closed` event is the confirming half of the story.
      reason = Reason::PeerClose;
      peer_closes_++;
    }
    s.active = false;
  }

  Record& r = push(Event::WsDisconnect);
  r.client_id = client_id;
  r.clients = clients;
  r.reason = reason;
  r.lifetime_ms = lifetime;
  r.pong_age_ms = pong_age;
  r.rx_msgs = rx;
  r.tx_msgs = tx;
  memcpy(r.ip, ip, sizeof(r.ip));
}

void noteWsError(uint8_t client_id, uint8_t clients) {
  if (client_id < MAX_SLOTS) slots_[client_id].last_error_ms = mono::now();
  Record& r = push(Event::WsError);
  r.client_id = client_id;
  r.clients = clients;
}

void noteWsStallReap(uint8_t client_id, uint32_t unwritable_ms, uint8_t clients) {
  stall_reaps_++;
  Record& r = push(Event::WsStallReap);
  r.client_id = client_id;
  r.clients = clients;
  r.value = unwritable_ms;
  // Carry the socket's history so a reap can be told apart from a merely idle client after
  // the fact: a slot with live traffic that suddenly stopped accepting writes is a peer that
  // went away, whereas one that never had any was probably never healthy.
  if (client_id < MAX_SLOTS) {
    const Slot& s = slots_[client_id];
    r.lifetime_ms = s.active ? (uint32_t)(mono::now() - s.connected_ms) : 0;
    r.pong_age_ms = s.last_pong_ms ? (uint32_t)(mono::now() - s.last_pong_ms) : 0;
    r.rx_msgs = s.rx_msgs;
    r.tx_msgs = s.tx_msgs;
    memcpy(r.ip, s.ip, sizeof(r.ip));
  }
}

void noteWsPong(uint8_t client_id) {
  if (client_id >= MAX_SLOTS) return;
  slots_[client_id].last_pong_ms = mono::now();
}

void noteWsRx(uint8_t client_id) {
  if (client_id >= MAX_SLOTS) return;
  Slot& s = slots_[client_id];
  s.last_rx_ms = mono::now();
  if (s.rx_msgs != 0xFFFF) s.rx_msgs++;
}

void noteWsTx() {
  for (uint8_t i = 0; i < MAX_SLOTS; i++) {
    if (slots_[i].active && slots_[i].tx_msgs != 0xFFFF) slots_[i].tx_msgs++;
  }
}

void noteWifi(bool up, int rssi) {
  if (up == wifi_up_) return;
  wifi_up_ = up;
  if (!up) wifi_disc_++;
  Record& r = push(up ? Event::WifiUp : Event::WifiDown);
  r.value = up ? (uint32_t)rssi : wifi_disc_;
}

// ---- ring access -------------------------------------------------------

uint32_t lastSeq() { return next_seq_ - 1; }

uint32_t firstSeq() {
  if (ring_count_ == 0) return 0;
  if (ring_count_ <= RING_CAPACITY) return 1;
  return next_seq_ - RING_CAPACITY;
}

const Record* bySeq(uint32_t seq) {
  if (seq == 0 || seq >= next_seq_) return nullptr;
  if (seq < firstSeq()) return nullptr;
  const Record& r = ring_[(seq - 1) % RING_CAPACITY];
  return r.seq == seq ? &r : nullptr;
}

// ---- serialisation -----------------------------------------------------

const char* eventName(Event e) {
  switch (e) {
    case Event::Boot: return "boot";
    case Event::WsConnect: return "ws_connect";
    case Event::WsDisconnect: return "ws_disconnect";
    case Event::WsError: return "ws_error";
    case Event::WsAtCap: return "ws_at_cap";
    case Event::LoopStall: return "loop_stall";
    case Event::HeapLow: return "heap_low";
    case Event::SocketLow: return "socket_low";
    case Event::WifiDown: return "wifi_down";
    case Event::WifiUp: return "wifi_up";
    case Event::ClockGlitch: return "clock_glitch";
    case Event::HeartbeatStall: return "heartbeat_stall";
    case Event::WsStallReap: return "ws_stall_reap";
    case Event::ApChange: return "ap_change";
  }
  return "unknown";
}

const char* reasonName(Reason r) {
  switch (r) {
    case Reason::None: return "";
    case Reason::PongTimeout: return "pong_timeout";
    case Reason::PeerClose: return "peer_close";
    case Reason::TransportError: return "transport_error";
    case Reason::Unknown: return "unclassified";
  }
  return "unknown";
}

void toJson(const Record& r, JsonObject obj) {
  obj["seq"] = r.seq;
  obj["t_ms"] = r.t_ms;
  obj["event"] = eventName(r.ev);
  if (r.reason != Reason::None) obj["reason"] = reasonName(r.reason);
  obj["heap"] = r.heap;
  obj["max_block"] = r.max_block;
  obj["spare_sockets"] = r.spare_sockets;
  obj["loop_max_ms"] = r.loop_max_ms;
  obj["http_max_ms"] = r.http_max_ms;
  obj["rssi"] = r.rssi;
  obj["clients"] = r.clients;
  if (r.value != 0) obj["value"] = r.value;

  if (r.ev == Event::WsConnect || r.ev == Event::WsDisconnect || r.ev == Event::WsError) {
    obj["client_id"] = r.client_id;
    if (r.ip[0] != '\0') obj["ip"] = r.ip;
  }
  if (r.ev == Event::WsDisconnect) {
    obj["lifetime_ms"] = r.lifetime_ms;
    obj["pong_age_ms"] = r.pong_age_ms;
    obj["rx_msgs"] = r.rx_msgs;
    obj["tx_msgs"] = r.tx_msgs;
  }
  if (r.ev == Event::LoopStall) {
    // Phase attribution, stashed in the shared numeric fields by noteLoop().
    obj["ota_ms"] = r.lifetime_ms;
    obj["http_ms"] = r.pong_age_ms;
    obj["ws_ms"] = r.rx_msgs;
  }
}

void healthToJson(JsonObject obj) {
  obj["uptime_s"] = mono::now() / 1000;
  obj["heap"] = ESP.getFreeHeap();
  obj["min_heap"] = ESP.getMinFreeHeap();
  obj["max_block"] = ESP.getMaxAllocHeap();
  obj["spare_sockets"] = spare_sockets_;
  obj["rssi"] = WiFi.isConnected() ? WiFi.RSSI() : 0;
  obj["wifi_disc"] = wifi_disc_;
  obj["pong_timeouts"] = pong_timeouts_;
  obj["stall_reaps"] = stall_reaps_;
  obj["bssid"] = bssid_;
  obj["wifi_roams"] = wifi_roams_;
  obj["peer_closes"] = peer_closes_;
  obj["transport_errors"] = transport_errors_;
  obj["loop_max_ms"] = loop_max_;
  obj["http_max_ms"] = http_max_;
  obj["ota_max_ms"] = ota_max_;
  obj["ws_max_ms"] = ws_max_;
  obj["loop_stalls"] = loop_stalls_;
  obj["loop_iters"] = loop_iters_;
  obj["loop_busy_ms"] = loop_busy_ms_;
  obj["clk_torn"] = mono::tornReads();
  // Reported for completeness but NOT alarmed on — see clockGlitchTotal(): on a firmware whose
  // bus task hammers mono::now(), this counts lost races, not clock faults (shq-suite-0039).
  obj["clk_back"] = mono::backwardReads();
  obj["clk_jump"] = mono::forwardJumps();
  obj["diag_seq"] = next_seq_ - 1;
}

// snprintf returns what it WOULD have written, so accumulating its return value directly walks
// `n` past `cap` and the next call computes a negative (huge, unsigned) remaining size.
static void appendClamped(char* out, size_t cap, size_t& n, int written) {
  if (written < 0) return;
  n += (size_t)written;
  if (n >= cap) n = cap > 0 ? cap - 1 : 0;
}

size_t renderText(char* out, size_t cap) {
  size_t n = 0;
  if (cap == 0) return 0;
  appendClamped(out, cap, n, snprintf(out + n, cap - n,
                "# diag uptime=%lus heap=%u minheap=%u maxblk=%u spare_sock=%u rssi=%d "
                "wifi_disc=%u pong_timeouts=%u peer_closes=%u transport_errors=%u "
                "loop_max=%ums http_max=%ums ota_max=%ums ws_max=%ums stalls=%u reaps=%u "
                "bssid=%s roams=%u seq=%u\n",
                (unsigned long)(mono::now() / 1000), (unsigned)ESP.getFreeHeap(),
                (unsigned)ESP.getMinFreeHeap(), (unsigned)ESP.getMaxAllocHeap(),
                (unsigned)spare_sockets_, WiFi.isConnected() ? WiFi.RSSI() : 0,
                (unsigned)wifi_disc_, (unsigned)pong_timeouts_, (unsigned)peer_closes_,
                (unsigned)transport_errors_, (unsigned)loop_max_, (unsigned)http_max_,
                (unsigned)ota_max_, (unsigned)ws_max_, (unsigned)loop_stalls_,
                (unsigned)stall_reaps_, bssid_, (unsigned)wifi_roams_,
                (unsigned)(next_seq_ - 1)));

  for (uint32_t seq = firstSeq(); seq <= lastSeq() && seq != 0; seq++) {
    const Record* r = bySeq(seq);
    if (r == nullptr) continue;
    if (n + 1 >= cap) break;
    appendClamped(out, cap, n, snprintf(out + n, cap - n,
                  "%u %lus %s%s%s id=%u ip=%s clients=%u val=%u life=%ums pong_age=%ums "
                  "rx=%u tx=%u heap=%u blk=%u sock=%u loop_max=%ums http_max=%ums rssi=%d\n",
                  (unsigned)r->seq, (unsigned long)(r->t_ms / 1000), eventName(r->ev),
                  r->reason != Reason::None ? ":" : "", reasonName(r->reason),
                  (unsigned)r->client_id, r->ip[0] ? r->ip : "-", (unsigned)r->clients,
                  (unsigned)r->value, (unsigned)r->lifetime_ms, (unsigned)r->pong_age_ms,
                  (unsigned)r->rx_msgs, (unsigned)r->tx_msgs, (unsigned)r->heap,
                  (unsigned)r->max_block, (unsigned)r->spare_sockets, (unsigned)r->loop_max_ms,
                  (unsigned)r->http_max_ms, (int)r->rssi));
  }
  return n;
}

uint32_t pongTimeouts() { return pong_timeouts_; }
uint32_t stallReaps() { return stall_reaps_; }
uint32_t wifiRoams() { return wifi_roams_; }
const char* currentBssid() { return bssid_; }
uint32_t peerCloses() { return peer_closes_; }
uint32_t transportErrors() { return transport_errors_; }
uint32_t loopStalls() { return loop_stalls_; }
uint32_t loopMaxMs() { return loop_max_; }
uint32_t httpMaxMs() { return http_max_; }
uint8_t spareSockets() { return spare_sockets_; }
uint32_t wifiDisconnects() { return wifi_disc_; }

}  // namespace diag
