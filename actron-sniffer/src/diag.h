// Self-diagnostics: why the WebSocket service drops, and what the device looked like when it did.
//
// Twin of somfy-sdn/src/diag.h (ported there in somfy fw 1.6.0) — keep the two in step, same
// standing rule as mono.{h,cpp}.
//
// WHY THIS EXISTS (ledger shq-suite-0038). After the millis()-wedge fix (shq-suite-0034) the
// actron bridge ran 48 h clean, then resumed flapping HA `unavailable` 4-8 times an hour. The
// existing telemetry could only say what was NOT wrong: `clk_*` all zero, `hb_age` inside its
// interval, `hb_tx` climbing, uptime unbroken — the firmware believed it was healthy throughout.
// ICMP proved the radio never left (2397/2398 replies over 40 min) while plain HTTP timed out on
// 16 of 151 requests, so the fault sits in the TCP/socket layer. Nothing on the device recorded
// WHY a socket died, so every diagnosis was inference from the far end.
//
// This module closes that gap. Every WS disconnect is classified from evidence held at the moment
// it fires — age of the last pong, whether a transport error preceded it, socket lifetime, traffic
// counts — and stamped with the machine's condition: free heap, largest allocatable block, spare
// lwIP sockets, and the worst main-loop stall in the window leading up to it. Records land in a RAM
// ring which is broadcast live to WS clients, REPLAYED TO THE NEXT CLIENT THAT CONNECTS (a client
// cannot receive news of its own disconnection), and served over HTTP at /diag and /diag.json.
//
// Reading the output, the two hypotheses this was built to separate:
//   * PongTimeout disconnects with a large `loop_max_ms` => the main loop stalls past the library's
//     ping/pong deadline and the server evicts a perfectly healthy HA client. Look at `http_ms` —
//     Arduino's WebServer::handleClient() blocks the whole loop while it waits on a slow request.
//   * Any disconnect with `spare_sockets` at 0 => the lwIP socket pool is exhausted; HTTP and WS
//     share it, so a leak in one starves the other.

#pragma once

#include <ArduinoJson.h>

#include <cstddef>
#include <cstdint>

#include "tcpsnap.h"
#include "ws_liveness.h"

namespace diag {

// Ring depth. Each Record is ~64 B, so 48 entries ≈ 3 kB of RAM held permanently. Sized to hold
// well over an hour of a healthy device's events, or ~15 min of a badly flapping one.
constexpr size_t RING_CAPACITY = 48;

// Main-loop period above which we record a LoopStall. Since fw 1.14.0 no keepalive deadline is
// shorter than ws_liveness::LIVENESS_MS (45 s), so a stall can no longer cost a session on its
// own; 1 s still catches the run-up to anything that would.
constexpr uint32_t LOOP_STALL_MS = 1000;

// Health telemetry push cadence over WS.
constexpr uint32_t HEALTH_INTERVAL_MS = 30000;

// hb_age beyond this emits a HeartbeatStall — twice ws_api's HEARTBEAT_INTERVAL_MS. Duplicated
// rather than included to keep diag free of any dependency on the WS layer it observes.
constexpr uint32_t HEARTBEAT_STALL_MS = 20000;

// Free heap below this emits a HeapLow event (once per crossing, re-armed by HEAP_LOW_CLEAR).
constexpr uint32_t HEAP_LOW_BYTES = 100000;
constexpr uint32_t HEAP_LOW_CLEAR_BYTES = 120000;

// Spare-socket probe: how many sockets we try to open to gauge headroom, and the level at or below
// which we emit SocketLow. lwIP's default pool on ESP32 is small (~10 across HTTP + WS + OTA).
constexpr uint8_t SOCKET_PROBE_MAX = 3;
constexpr uint8_t SOCKET_LOW_SPARE = 1;

// Keepalive deadlines live in ws_liveness.h (fw 1.14.0): the guard that enforces them and this
// classifier read the same constants, so they can never drift apart. The library's own pong
// reaper (`enableHeartbeat`) is no longer used — an eviction is now something the guard TELLS
// diag about (noteWsEvict / noteWsStallReap), not something inferred from a pong age.
constexpr uint32_t WS_PING_INTERVAL_MS = ws_liveness::PING_INTERVAL_MS;

// Inbound traffic this recent at disconnect means the peer was demonstrably alive and talking, so
// the close came from its end.
constexpr uint32_t PEER_ACTIVE_MS = 10000;

enum class Event : uint8_t {
  Boot = 0,          // first record after reset; `value` = esp_reset_reason()
  WsConnect,         // a client completed the handshake
  WsDisconnect,      // a client went away — see `reason`
  WsError,           // WStype_ERROR from the library
  WsAtCap,           // all WEBSOCKETS_SERVER_CLIENT_MAX slots occupied; new clients are refused
  LoopStall,         // one main-loop iteration took >= LOOP_STALL_MS; `value` = duration
  HeapLow,           // free heap fell below HEAP_LOW_BYTES; `value` = free heap
  SocketLow,         // spare lwIP sockets at or below SOCKET_LOW_SPARE; `value` = spare count
  WifiDown,          // association lost
  WifiUp,            // association regained; `value` = RSSI
  ClockGlitch,       // mono::Filter caught a torn/backward/far-future millis() read
  HeartbeatStall,    // hb_age exceeded 2x HEARTBEAT_INTERVAL_MS — the shq-suite-0034 signature
  WsStallReap,       // a socket refused writes for WS_STALL_REAP_MS and was dropped;
                     // `value` = how long it had been unwritable (shq-suite-0038)
  ApChange,          // the station associated to a different BSSID; `value` = lifetime roam
                     // count, `ip` = the NEW bssid, previous one in the serial log
  NetRecover,        // the network-stack watchdog acted (fw 1.11.0, shq-suite-0044); `ip` =
                     // the reason ("clock-step", "unreachable", "heap-low"), `value` =
                     // re-associations so far.
  WsEvict,           // the liveness policy dropped a silent client (fw 1.14.0, ws_liveness.h):
                     // nothing inbound for LIVENESS_MS (hard cap while retransmitting);
                     // `value` = silence ms, `reason` = PongTimeout, tcp = the live pcb
};

// Why a socket died. Inferred, and deliberately conservative: `Unknown` is preferred to a
// confident wrong answer, and every record carries the raw evidence so the inference can be
// re-judged later without a reflash.
enum class Reason : uint8_t {
  None = 0,
  PongTimeout,       // OBSERVED (fw 1.14.0): our liveness policy evicted it — nothing inbound
                     // (pong, ping or text) for ws_liveness::LIVENESS_MS, hard cap 120 s
  PeerClose,         // traffic was flowing right up to the end => the CLIENT hung up
  TransportError,    // a WStype_ERROR arrived for this slot immediately beforehand
  Unknown,           // quiet socket, not evicted by us — nothing to pin it on
  StallReap,         // OBSERVED: the write-guard dropped it for staying unwritable
};

struct Record {
  uint32_t seq;            // monotonic, never reused; clients use it to spot gaps
  uint32_t t_ms;           // mono::now() at capture
  Event ev;
  Reason reason;           // WsDisconnect only
  uint8_t client_id;
  uint8_t clients;         // connected clients immediately after the event
  uint32_t value;          // event-specific scalar (see the Event comments above)
  uint32_t lifetime_ms;    // WsDisconnect: how long the socket lived
  uint32_t pong_age_ms;    // WsDisconnect: age of the last pong received on it
  uint16_t rx_msgs;        // WsDisconnect: frames received on it
  uint16_t tx_msgs;        // WsDisconnect: state pushes sent while it was up
  uint32_t heap;           // free heap at capture
  uint32_t max_block;      // largest allocatable block at capture (heap fragmentation)
  uint32_t loop_max_ms;    // worst main-loop iteration since the previous record
  uint32_t http_max_ms;    // worst WebServer::handleClient() since the previous record
  uint8_t spare_sockets;   // lwIP headroom at capture, 0..SOCKET_PROBE_MAX
  int8_t rssi;
  char ip[16];             // WsConnect/WsDisconnect: peer address
  // MAC-layer transmit picture of the run-up (fw 1.12.0, txstats.h): deltas since the PREVIOUS
  // record, saturating at 65535. A disconnect with tx_retry/tx_to climbing against a flat tx_ok
  // is the uplink failing on the air; all zero is the MAC not even being asked.
  uint16_t tx_ok;          // frames acknowledged
  uint16_t tx_retry;       // EDCA + TB retransmissions
  uint16_t tx_to;          // ACK/BA timeouts
  uint16_t tx_fail;        // failure-state matrix entries
  // WsDisconnect / WsStallReap: lwIP's own view of the connection (tcpsnap.h). At a reap it is
  // captured live before the socket is closed; at a disconnect it is the last 1 Hz sample.
  tcpsnap::Snap tcp;
};

// ---- lifecycle ---------------------------------------------------------

// Call once at the end of setup(), after WiFi. Emits the Boot record with the reset reason.
void begin();

// Call once per Arduino loop() iteration with the phase timings, in milliseconds. `total` is the
// whole iteration; the rest attribute it. Splitting them is the point: a stall inside
// handleClient() means a slow HTTP peer is blocking the WS pump, which is a completely different
// fault from the WS pump itself being slow.
void noteLoop(uint32_t total_ms, uint32_t ota_ms, uint32_t http_ms, uint32_t ws_ms);

// Periodic housekeeping — socket probe, heap/clock/heartbeat watches. Safe to call every loop;
// the work inside is rate-limited.
void tick(uint32_t hb_age_ms);

// ---- WS lifecycle hooks (called from ws_api's onEvent) -----------------

void noteWsConnect(uint8_t client_id, const char* ip, uint8_t clients);
void noteWsDisconnect(uint8_t client_id, uint8_t clients);
void noteWsError(uint8_t client_id, uint8_t clients);
void noteWsPong(uint8_t client_id);      // pong received => the peer is alive

// A slot was dropped by the write-guard because its socket stayed unwritable. This is the
// stall that never happened: writing to it would have blocked the main loop ~10 s inside the
// Arduino core's write retry loop (see ws_guard.h).
void noteWsStallReap(uint8_t client_id, uint32_t unwritable_ms, uint8_t clients,
                     const tcpsnap::Snap* tcp);
// The liveness policy dropped a slot for silence (fw 1.14.0): `why` is the rule that fired
// ("silent" / "silent-hard"), `silence_ms` the evidence, `tcp` the live pcb. Marks the slot so
// the WsDisconnect that follows is classified as OBSERVED PongTimeout rather than inferred.
void noteWsEvict(uint8_t client_id, const char* why, uint32_t silence_ms, uint8_t clients,
                 const tcpsnap::Snap* tcp);
void noteWsRx(uint8_t client_id);        // any inbound frame => the peer is alive and talking
void noteWsTx();                         // a state push went to every client

// The 1 Hz lwIP pcb sample for a connected slot (from ws_api::loop). Kept per slot so a
// disconnect record — which fires after the library has already closed the socket — can still
// carry the last picture TCP had of it.
void noteTcp(uint8_t client_id, const tcpsnap::Snap& snap);

// Read-side of the per-slot bookkeeping, for the liveness policy in ws_guard (fw 1.14.0).
// slotAges() fills the age of the last pong and of the last inbound frame of any kind; returns
// false (outputs untouched) for a slot diag does not know. lastTcp() is the last 1 Hz sample,
// nullptr for an unknown slot.
bool slotAges(uint8_t client_id, uint32_t now_ms, uint32_t* pong_age_ms, uint32_t* rx_age_ms);
const tcpsnap::Snap* lastTcp(uint8_t client_id);

void noteWifi(bool up, int rssi);

// The network-stack watchdog is about to re-associate for `reason`; `recoveries` is its
// lifetime re-association count. Recorded BEFORE the action so the heap/rssi stamp shows the
// state that triggered it.
void noteNetRecover(const char* reason, uint32_t recoveries);

// ---- ring access -------------------------------------------------------

// Sequence of the newest record (0 if the ring is empty). ws_api polls this rather than being
// called back, so nothing is ever emitted from inside the WS library's own event dispatch.
uint32_t lastSeq();

// Oldest sequence still held. Records older than this were overwritten; a client that sees a gap
// between this and its own cursor knows it missed some.
uint32_t firstSeq();

// Fetch by sequence. Returns nullptr if that record has been overwritten or does not exist yet.
const Record* bySeq(uint32_t seq);

// ---- serialisation -----------------------------------------------------

// Render one record into an existing JSON object (as emitted in a `diag` message).
void toJson(const Record& r, JsonObject obj);

// The health snapshot, in three parts so no WS frame exceeds ws_liveness::FRAME_BUDGET_BYTES
// (fw 1.14.0): core vitals (`health`), socket/liveness counters + lwIP's view (`health_ws`),
// netwatch + MAC transmit telemetry (`health_net`). healthToJson() is all three in one object
// for /diag.json only — at ~1.3 kB it must never go over WS again (shq-suite-0046).
void healthCoreToJson(JsonObject obj);
void healthWsToJson(JsonObject obj);
void healthNetToJson(JsonObject obj);
void healthToJson(JsonObject obj);

// Human-readable dump for GET /diag: a health line followed by the ring, newest last.
size_t renderText(char* out, size_t cap);

const char* eventName(Event e);
const char* reasonName(Reason r);

// Counters surfaced in /stats and in the health payload.
uint32_t pongTimeouts();
uint32_t peerCloses();
uint32_t stallReaps();
uint32_t wifiRoams();

// Current BSSID as a printable string, or "-" when not associated. The station is the ONLY
// authoritative source for which AP is serving this device — the UniFi controller's client list
// has been observed reporting a different AP than the station itself (wiki estate/shq-network.md).
const char* currentBssid();
uint32_t transportErrors();
uint32_t loopStalls();
uint32_t loopMaxMs();       // worst main-loop iteration since boot
uint32_t httpMaxMs();       // worst handleClient() since boot
uint8_t spareSockets();     // most recent probe result
uint32_t wifiDisconnects();

}  // namespace diag
