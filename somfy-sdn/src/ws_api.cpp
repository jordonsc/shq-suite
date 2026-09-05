#include "ws_api.h"

#include <Arduino.h>
#include <ArduinoJson.h>
#include <WebSocketsServer.h>
#include <WiFi.h>

#include <cstring>

#include "bus.h"
#include "devices.h"
#include "diag.h"
#include "fault.h"
#include "mono.h"
#include "sdn.h"
#include "tcpsnap.h"
#include "ws_guard.h"
#include "ws_liveness.h"
#include "version.h"
#include "wifi_proto.h"
#include "wifi_prov.h"

namespace ws_api {

namespace {

GuardedWebSocketsServer* g_server = nullptr;
volatile bool g_dirty = false;
uint32_t g_last_heartbeat_ms = 0;

// Wedge watchdog. WebSocketsServer has a hard WEBSOCKETS_SERVER_CLIENT_MAX (5) client cap. A
// buggy/duplicate client can fill every slot with live-but-idle connections — each kept alive by
// its own WS keepalive, so the heartbeat reaper (enableHeartbeat) never evicts them — after which
// the library refuses all new handshakes (accept then drop with no HTTP response) and the device
// looks dead to HA while HTTP and the SDN bus stay perfectly healthy. A controller only ever has
// ONE legitimate HA coordinator, so sitting at the full client cap for minutes is an unambiguous
// wedge: self-heal by rebooting (WiFi creds + motor config live in NVS; the device re-announces
// over zeroconf and HA reconnects). The HA client also got a close-before-reconnect fix that stops
// the leak at source — this is the firmware backstop so no future client can wedge us permanently.
constexpr uint32_t WEDGE_REBOOT_MS = 5 * 60 * 1000;  // continuously at capacity this long => reboot
uint32_t g_at_capacity_since_ms = 0;                 // millis() when we hit the cap; 0 = below cap

// Lifetime WS event counters (fw 1.5.0, ported from actron-sniffer — see ws_api.h).
uint32_t g_conn_events = 0;
uint32_t g_disc_events = 0;
uint32_t g_err_events = 0;

// Lifetime count of state broadcasts pushed to clients (fw 1.5.1). Paired with hb_age in /stats
// this is the one-curl answer to "is the heartbeat still running?" — the question that cost a
// morning of packet captures the first time (ledger shq-suite-0034).
uint32_t g_hb_broadcasts = 0;

// Diagnostics fan-out cursor (ledger shq-suite-0038). diag records are POLLED from loop() rather
// than pushed from a callback: every record originates inside the WS library's own event dispatch,
// and emitting a frame from there would re-enter the server mid-iteration.
// Per-client replay cursor (fw 1.14.0): the newest diag seq already sent to that slot. Set to
// the start of the backlog on connect, so the backlog and the live stream are ONE mechanism — a
// record per frame, at most one per DIAG_DRAIN_SPACING_MS per client, each held back while the
// socket would block or the pcb is retransmitting. The old `diag_backlog` frame (12 records, ~2.6 kB,
// two full-MSS segments) was the largest frame this firmware ever sent and the pcap showed it
// lost on every attempt on the affected boards — a reconnect could not succeed on exactly the
// units that needed one, which is why the churn was self-perpetuating (shq-suite-0046).
uint32_t g_diag_cursor[WEBSOCKETS_SERVER_CLIENT_MAX] = {0};
uint32_t g_last_health_ms = 0;
uint8_t g_health_phase = 0;  // 0 = idle; 1..3 = which of the three health frames goes next
uint32_t g_health_phase_ms = 0;
uint32_t g_last_tcp_sample_ms = 0;

// Records replayed to a client the moment it connects (see g_diag_cursor).
constexpr uint32_t DIAG_BACKLOG_MAX = 12;
// Minimum spacing between two diag frames to ONE client. The frame budget is per WS frame, but
// lwIP merges writes: CONFIG_LWIP_TCP_OVERSIZE_MSS=y gives every unsent segment a full-MSS pbuf
// that later writes FILL while it waits for cwnd (1-2 MSS on a fresh socket) or an ACK. Two
// records per loop pass therefore left the device as 1436 B segments on every reconnect — the
// very size class the pcap showed lost. 250 ms is far above the LAN RTT, so each record is
// ACKed before the next is written and stays its own segment.
constexpr uint32_t DIAG_DRAIN_SPACING_MS = 250;
uint32_t g_diag_sent_ms[WEBSOCKETS_SERVER_CLIENT_MAX] = {0};  // mono::now() of the last diag frame per slot

const char* movementName(sdn::MovementState m) {
  switch (m) {
    case sdn::MovementState::MOVING_UP: return "up";
    case sdn::MovementState::MOVING_DOWN: return "down";
    default: return "idle";
  }
}

void buildState(JsonDocument& doc) {
  doc["type"] = "state";
  JsonObject data = doc["data"].to<JsonObject>();
  data["mode"] = (bus::mode() == bus::Mode::ACTIVE) ? "active" : "listen";
  // Controller identity/health — the HA component names the controller device off `mac` (stable
  // across DHCP changes, unlike the IP) and surfaces fw/ip/rssi diagnostics.
  data["fw"] = SOMFY_FW_VERSION;
  data["mac"] = WiFi.macAddress();
  data["hostname"] = wifi_prov::hostname();
  data["ip"] = WiFi.localIP().toString();
  data["rssi"] = WiFi.isConnected() ? WiFi.RSSI() : 0;

  JsonArray arr = data["devices"].to<JsonArray>();
  devices::DeviceTable& t = bus::table();
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (d == nullptr) continue;
    JsonObject o = arr.add<JsonObject>();
    char addr[9];
    sdn::formatAddress(d->addr, addr);
    o["addr"] = addr;
    if (d->label[0] != '\0') o["label"] = d->label;
    // Native Somfy % (0=open .. 100=closed); HA inverts. Null when unknown / faulted.
    if (d->position_known && !d->fault) {
      o["position"] = d->position_pct;
      o["pulses"] = d->position_pulses;  // absolute encoder count (provisioning aid)
    } else {
      o["position"] = nullptr;
      o["pulses"] = nullptr;
    }
    o["moving"] = movementName(d->movement);
    o["fault"] = d->fault;
    o["status"] = d->status[0] ? d->status : "ok";  // human-readable problem, or "ok"
    o["online"] = d->online;
    // Only report limits that are actually programmed (0xFFFF = unset).
    if (d->limits_known) {
      if (d->up_limit_pulses != sdn::POSITION_UNKNOWN) o["up_limit"] = d->up_limit_pulses;
      if (d->down_limit_pulses != sdn::POSITION_UNKNOWN) o["down_limit"] = d->down_limit_pulses;
    }
    if (d->direction_known) {
      o["direction"] = (d->direction == sdn::DIRECTION_REVERSED) ? "reversed" : "normal";
    }
  }
  data["errors_recent"] = bus::errors().total();
}

void broadcastState() {
  if (g_server == nullptr) return;
  g_hb_broadcasts++;
  diag::noteWsTx();
  JsonDocument doc;
  buildState(doc);
  String out;
  serializeJson(doc, out);
  g_server->broadcastWritableTXT(out);
}

void sendStateTo(uint8_t client) {
  JsonDocument doc;
  buildState(doc);
  String out;
  serializeJson(doc, out);
  g_server->sendWritableTXT(client, out);
}

void sendAck(uint8_t client, const char* id) {
  JsonDocument doc;
  doc["type"] = "ack";
  if (id) doc["id"] = id;
  doc["status"] = "accepted";
  String out;
  serializeJson(doc, out);
  g_server->sendWritableTXT(client, out);
}

void sendError(uint8_t client, const char* id, const char* msg) {
  JsonDocument doc;
  doc["type"] = "error";
  if (id) doc["id"] = id;
  doc["message"] = msg;
  String out;
  serializeJson(doc, out);
  g_server->sendWritableTXT(client, out);
}

// Resolve the target addr for a command. Returns false (and sets *err) when required + invalid.
bool resolveAddr(JsonObject& cmd, uint8_t out[3], const char** err) {
  const char* a = cmd["addr"];
  if (a == nullptr) { *err = "missing addr"; return false; }
  if (!sdn::parseAddress(a, out)) { *err = "invalid addr"; return false; }
  return true;
}

// Returns true if accepted (and enqueues a command); false sets *err.
bool handleMotorCommand(const char* command, JsonObject& cmd, const char** err) {
  uint8_t addr[3];
  if (!resolveAddr(cmd, addr, err)) return false;

  bus::Command c;
  memcpy(c.addr, addr, 3);
  c.job_id = bus::nextJobId();

  if (strcmp(command, "open") == 0) {
    c.type = bus::CmdType::OPEN;
  } else if (strcmp(command, "close") == 0) {
    c.type = bus::CmdType::CLOSE;
  } else if (strcmp(command, "stop") == 0) {
    c.type = bus::CmdType::STOP;
  } else if (strcmp(command, "set_position") == 0) {
    if (!cmd["position"].is<int>() && !cmd["position"].is<float>()) {
      *err = "position must be number"; return false;
    }
    int ha = cmd["position"].as<int>();
    if (ha < 0 || ha > 100) { *err = "position out of range (0..100)"; return false; }
    c.type = bus::CmdType::SET_POSITION;
    c.u8 = sdn::haToSomfy((uint8_t)ha);  // HA position -> native Somfy %
  } else if (strcmp(command, "jog") == 0) {
    // Timed momentary nudge (CTRL_MOVE) — commissioning move that works before limits are set.
    const char* dir = cmd["direction"];
    if (dir == nullptr) { *err = "missing direction"; return false; }
    bool up = (strcmp(dir, "up") == 0);
    bool down = (strcmp(dir, "down") == 0);
    if (!up && !down) { *err = "direction must be up/down"; return false; }
    int dur = cmd["duration"].is<int>() ? cmd["duration"].as<int>() : 20;
    if (dur < sdn::MOVE_DURATION_MIN) dur = sdn::MOVE_DURATION_MIN;
    if (dur > 255) dur = 255;
    c.type = bus::CmdType::MOVE_TIMED;
    c.u8 = up ? 1 : 0;
    c.u16 = (uint16_t)dur;
  } else if (strcmp(command, "move_steps") == 0) {
    const char* dir = cmd["direction"];
    if (dir == nullptr) { *err = "missing direction"; return false; }
    bool up = (strcmp(dir, "up") == 0);
    bool down = (strcmp(dir, "down") == 0);
    if (!up && !down) { *err = "direction must be up/down"; return false; }
    if (!cmd["pulses"].is<int>()) { *err = "pulses must be int"; return false; }
    int pulses = cmd["pulses"].as<int>();
    if (pulses < 0 || pulses > 65535) { *err = "pulses out of range"; return false; }
    c.type = bus::CmdType::MOVE_STEPS;
    c.u8 = up ? sdn::MOVEOF_PULSES_UP : sdn::MOVEOF_PULSES_DOWN;
    c.u16 = (uint16_t)pulses;
  } else if (strcmp(command, "set_top_limit") == 0) {
    c.type = bus::CmdType::SET_TOP_LIMIT;
  } else if (strcmp(command, "set_bottom_limit") == 0) {
    c.type = bus::CmdType::SET_BOTTOM_LIMIT;
  } else if (strcmp(command, "set_bottom_limit_pulses") == 0) {
    if (!cmd["pulses"].is<int>()) { *err = "pulses must be int"; return false; }
    int p = cmd["pulses"].as<int>();
    if (p < 0 || p > 65535) { *err = "pulses out of range"; return false; }
    c.type = bus::CmdType::SET_BOTTOM_LIMIT_AT;
    c.u16 = (uint16_t)p;
  } else if (strcmp(command, "set_direction") == 0) {
    if (!cmd["reversed"].is<bool>()) { *err = "reversed must be bool"; return false; }
    c.type = bus::CmdType::SET_DIRECTION;
    c.u8 = cmd["reversed"].as<bool>() ? 1 : 0;
  } else if (strcmp(command, "reset") == 0) {
    c.type = bus::CmdType::RESET;  // execCommand defaults to RESET_LIMITS
  } else if (strcmp(command, "forget") == 0) {
    c.type = bus::CmdType::FORGET;  // local table removal — allowed in any mode
  } else if (strcmp(command, "identify") == 0) {
    c.type = bus::CmdType::IDENTIFY;
  } else {
    *err = "unknown command";
    return false;
  }

  if (c.type != bus::CmdType::FORGET && bus::mode() != bus::Mode::ACTIVE) {
    *err = "bus is in LISTEN mode (set_mode active first)"; return false;
  }
  if (!bus::enqueue(c)) { *err = "command queue full"; return false; }
  return true;
}

void handleCommand(uint8_t client, JsonDocument& doc) {
  const char* type = doc["type"];
  const char* id = doc["id"];
  if (type == nullptr || strcmp(type, "command") != 0) {
    sendError(client, id, "expected type=command");
    return;
  }
  const char* command = doc["command"];
  if (command == nullptr) {
    sendError(client, id, "missing command");
    return;
  }
  JsonObject obj = doc.as<JsonObject>();
  const char* err = "unknown command";

  // Admin commands (no addr).
  if (strcmp(command, "set_mode") == 0) {
    const char* m = obj["mode"];
    if (m == nullptr) { sendError(client, id, "missing mode"); return; }
    if (strcmp(m, "active") == 0) bus::setMode(bus::Mode::ACTIVE);
    else if (strcmp(m, "listen") == 0) bus::setMode(bus::Mode::LISTEN);
    else { sendError(client, id, "mode must be listen/active"); return; }
    sendAck(client, id);
    notifyStateChanged();
    return;
  }
  if (strcmp(command, "rediscover") == 0) {
    if (bus::mode() != bus::Mode::ACTIVE) { sendError(client, id, "set_mode active first"); return; }
    bus::Command c;
    c.type = bus::CmdType::REDISCOVER;
    c.broadcast = true;
    c.job_id = bus::nextJobId();
    if (!bus::enqueue(c)) { sendError(client, id, "command queue full"); return; }
    sendAck(client, id);
    return;
  }
  if (strcmp(command, "reboot") == 0) {
    // Ack first so HA sees the command land — the reply has to clear the socket before the stack
    // goes down. wifi_prov::noteReboot() persists the reason, so the next boot's /stats `note=`
    // says this was deliberate rather than a crash.
    sendAck(client, id);
    delay(100);
    wifi_prov::noteReboot("ws-command");
    return;
  }
  if (strcmp(command, "reconnect_wifi") == 0) {
    // Ack first; wifi_prov drops + re-scans from loop() once this reply has flushed. The link
    // bounce drops this WS connection — the HA coordinator reconnects (heartbeat-reaped) and the
    // next snapshot carries the new rssi.
    sendAck(client, id);
    wifi_prov::requestReconnectBestAp();
    return;
  }
  if (strcmp(command, "set_wifi_proto") == 0) {
    // fw 1.13.0 WiFi protocol A/B knob (ledger shq-suite-0046). Persist, ack, then REBOOT
    // (fw 1.14.0): esp_wifi_set_protocol() is honoured only on a fresh WiFi init — a live
    // re-association left the canary negotiating HE20 with `bgn` persisted, while a reboot came
    // up HT20. The reason lands in the next boot's note= so the restart is not read as a crash.
    const char* p = obj["proto"];
    wifi_proto::Proto proto;
    if (p == nullptr || !wifi_proto::parse(p, &proto)) {
      sendError(client, id, "proto must be bgnax|bgn|bg");
      return;
    }
    if (!wifi_prov::setWifiProto(proto)) { sendError(client, id, "nvs write failed"); return; }
    sendAck(client, id);
    delay(100);
    wifi_prov::noteReboot("wifiproto");
  }

  if (handleMotorCommand(command, obj, &err)) {
    sendAck(client, id);
  } else {
    sendError(client, id, err);
  }
}

// ---- diagnostics fan-out ------------------------------------------------

// One record, one frame (<= ~554 B with the tcp_* keys), to one client, through the telemetry
// gate: skipped while the socket would block or the pcb is retransmitting. Returns false when
// held back so the caller's cursor stays put and the record is retried next pass.
bool sendDiagRecordTo(uint8_t client, const diag::Record& r) {
  JsonDocument doc;
  doc["type"] = "diag";
  JsonObject obj = doc["event"].to<JsonObject>();
  diag::toJson(r, obj);
  String out;
  serializeJson(doc, out);
  return (*g_server).sendTelemetryTXT(client, out);
}

// Point a fresh client's cursor at the start of its backlog — the last DIAG_BACKLOG_MAX records
// (or fewer) — for drainDiag() to deliver one frame at a time. A socket can never be told about
// its own death, so this is the ONLY way HA learns why the previous session ended. HA keys on
// `seq`, so a record a second client already received live is simply discarded there.
void startDiagBacklog(uint8_t client) {
  if (client >= WEBSOCKETS_SERVER_CLIENT_MAX) return;
  const uint32_t last = diag::lastSeq();
  uint32_t first = diag::firstSeq();
  if (last == 0 || first == 0) { g_diag_cursor[client] = last; return; }
  if (last - first + 1 > DIAG_BACKLOG_MAX) first = last - DIAG_BACKLOG_MAX + 1;
  g_diag_cursor[client] = first - 1;
  g_diag_sent_ms[client] = 0;
}

// Advance every connected client's cursor towards the ring head, one frame per
// DIAG_DRAIN_SPACING_MS. The cursor only moves on a successful queue, so a held-back frame is
// retried, not lost.
void drainDiag(uint32_t now) {
  const uint32_t newest = diag::lastSeq();
  const uint32_t oldest = diag::firstSeq();
  for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
    if (!(*g_server).connected(i)) continue;
    if (oldest > 0 && g_diag_cursor[i] + 1 < oldest) g_diag_cursor[i] = oldest - 1;  // the ring wrapped past us
    if (g_diag_cursor[i] >= newest) continue;
    if (g_diag_sent_ms[i] != 0 && (uint32_t)(now - g_diag_sent_ms[i]) < DIAG_DRAIN_SPACING_MS) continue;
    const diag::Record* r = diag::bySeq(g_diag_cursor[i] + 1);
    if (r != nullptr) {
      if (!sendDiagRecordTo(i, *r)) continue;
      g_diag_sent_ms[i] = (now == 0) ? 1u : now;  // 0 is the "never" sentinel
    }
    g_diag_cursor[i]++;
  }
}

// The health push is THREE frames, a second apart, each under ws_liveness::FRAME_BUDGET_BYTES:
//   health      device vitals + WS-layer counters + fault + fw
//   health_ws   guard/liveness counters + lwIP's view of the HA socket
//   health_net  netwatch + MAC transmit telemetry + PHY/protocol
// The single ~1.3 kB `health` frame was the largest periodic thing this firmware sent, and the
// one the pcap showed being lost on the affected boards (shq-suite-0046). HA merges the three
// into one dict, so every sensor reads exactly as before.
void sendHealthFrame(uint8_t phase) {
  JsonDocument doc;
  JsonObject obj = doc["data"].to<JsonObject>();
  if (phase == 1) {
    doc["type"] = "health";
    diag::healthCoreToJson(obj);
    // WS-layer counters live here, not in diag, so diag stays independent of the server it watches.
    obj["clients"] = (*g_server).connectedClients();
    obj["ws_conn"] = g_conn_events;
    obj["ws_disc"] = g_disc_events;
    obj["ws_err"] = g_err_events;
    obj["hb_age_ms"] = (uint32_t)(mono::now() - g_last_heartbeat_ms);
    obj["hb_tx"] = g_hb_broadcasts;
    // Device-level fault (fw 1.10.0, ledger shq-suite-0041). "ok" when clear. This is the signal
    // that was missing when a clock wedged for nine hours with every indicator green.
    obj["fault"] = fault::registry().worstSlug();
    obj["fault_detail"] = fault::registry().worstDetail();
    obj["fault_mask"] = fault::registry().mask();
    obj["fw"] = SOMFY_FW_VERSION " " __DATE__ " " __TIME__;
  } else if (phase == 2) {
    doc["type"] = "health_ws";
    diag::healthWsToJson(obj);
    // Write-guard + liveness counters (ws_guard.h). skipped/deferred separate "briefly busy,
    // skipped one frame, recovered" from "socket died"; liveness_extended counts the holes the
    // policy rode out instead of evicting; big_frames must stay 0.
    obj["skipped_writes"] = (*g_server).skippedWrites();
    obj["deferred_reaps"] = (*g_server).deferredReaps();
    obj["liveness_evicts"] = (*g_server).livenessEvicts();
    obj["liveness_extended"] = (*g_server).livenessExtensions();
    obj["deferred_telemetry"] = (*g_server).deferredTelemetry();
    obj["pings"] = (*g_server).pingsSent();
    obj["ping_skips"] = (*g_server).pingSkips();
    obj["big_frames"] = (*g_server).bigFrames();
  } else {
    doc["type"] = "health_net";
    diag::healthNetToJson(obj);
  }
  String out;
  serializeJson(doc, out);
  (*g_server).broadcastTelemetryTXT(out);
}

void onEvent(uint8_t client, WStype_t type, uint8_t* payload, size_t length) {
  switch (type) {
    case WStype_CONNECTED: {
      g_conn_events++;
      wifi_prov::noteInbound();  // a completed handshake is inbound traffic (netwatch)
      const IPAddress ip = g_server->remoteIP(client);
      diag::noteWsConnect(client, ip.toString().c_str(), g_server->connectedClients());
      sendStateTo(client);
      // After the snapshot, so a client always has state before it has history. The backlog is
      // drained a record per frame from loop(), never blasted as one frame (see g_diag_cursor).
      startDiagBacklog(client);
      break;
    }
    case WStype_DISCONNECTED:
      g_disc_events++;
      diag::noteWsDisconnect(client, g_server->connectedClients());
      break;
    case WStype_ERROR:
      g_err_events++;
      diag::noteWsError(client, g_server->connectedClients());
      break;
    case WStype_PONG:
      // Proof the peer answered our reaper's ping. Its absence is what gets a client evicted, so
      // the age of the last one is the primary evidence in a disconnect classification.
      diag::noteWsPong(client);
      wifi_prov::noteInbound();
      break;
    case WStype_PING:
      diag::noteWsRx(client);
      wifi_prov::noteInbound();
      break;
    case WStype_TEXT: {
      diag::noteWsRx(client);
      wifi_prov::noteInbound();
      JsonDocument doc;
      DeserializationError e = deserializeJson(doc, payload, length);
      if (e) { sendError(client, nullptr, "invalid JSON"); return; }
      handleCommand(client, doc);
      break;
    }
    default:
      break;
  }
}

}  // namespace

void begin(uint16_t port) {
  g_server = new GuardedWebSocketsServer(port);
  g_server->begin();
  g_server->onEvent(onEvent);
  // NO enableHeartbeat() (fw 1.14.0, ledger shq-suite-0046). The library's keepalive pinged the
  // socket unguarded from inside loop() and evicted a client 10 s after the first missed pong —
  // inside lwIP's retransmit ladder, so a 10 s uplink fade became a dead session (42 sessions
  // recorded at pong_age = 25.0 s exactly). The half-open-zombie case it was added for in
  // fw 1.1.5 is still covered: GuardedWebSocketsServer pings through the write-guard and judges
  // liveness on the policy in ws_liveness.h (silence in BOTH directions for 45 s, extended while
  // the pcb is retransmitting, hard cap 120 s; unwritable 30 s).
  g_last_heartbeat_ms = mono::now();
  g_last_health_ms = g_last_heartbeat_ms;
}

void loop() {
  if (g_server == nullptr) return;
  // Judge BEFORE loop() (fw 1.8.0's ordering, kept): a slot the policy drops must be gone before
  // the library touches it. Since fw 1.14.0 no library write is unguarded — the keepalive ping
  // below goes through the guard and the library's own heartbeat is off — so this ordering is
  // hygiene rather than the 10 s core-write defence it used to be (ws_liveness.h).
  g_server->judge(mono::now());

  g_server->loop();

  // Our keepalive, after loop() so a pong that just arrived is already on the books.
  g_server->sendPings(mono::now());

  if (g_dirty) {
    g_dirty = false;
    broadcastState();
    g_last_heartbeat_ms = mono::now();
  }

  uint32_t t = mono::now();
  // UNSIGNED elapsed-time test, deliberately (fw 1.5.1, ledger shq-suite-0034). This was
  // `(int32_t)(t - g_last_heartbeat_ms) >= (int32_t)HEARTBEAT_INTERVAL_MS`, which reads as
  // "not due yet" whenever the stamp sits in the future — and an occasional far-future millis()
  // read put it there, wedging the heartbeat for 8 h on Bed 2. HA then sees a device that
  // accepts connections and answers WS pings but never pushes state: available for one snapshot,
  // unavailable 30 s later, forever, at a 40 s cadence. Unsigned subtraction wraps a
  // future stamp to a huge elapsed value, so the very next loop fires a broadcast and re-stamps
  // it — the failure self-corrects in 10 ms instead of lasting until the clock catches up.
  // mono::now() (above) is the second layer: it filters the bad reads out in the first place.
  if ((uint32_t)(t - g_last_heartbeat_ms) >= HEARTBEAT_INTERVAL_MS) {
    broadcastState();
    g_last_heartbeat_ms = t;
  }

  // Drain diagnostics to every listener, a record per frame, paced per client so a reconnect
  // burst is neither a loop stall nor a run of coalesced full-MSS segments.
  if (g_server->connectedClients() > 0) {
    drainDiag(t);
    // Three health frames a second apart (sendHealthFrame), never in one pass.
    if (g_health_phase == 0 && (uint32_t)(t - g_last_health_ms) >= diag::HEALTH_INTERVAL_MS) {
      g_last_health_ms = t;
      g_health_phase = 1;
      g_health_phase_ms = t - 1000;  // the first frame goes out now
    }
    if (g_health_phase != 0 && (uint32_t)(t - g_health_phase_ms) >= 1000) {
      sendHealthFrame(g_health_phase);
      g_health_phase_ms = t;
      g_health_phase = (g_health_phase >= 3) ? 0 : (uint8_t)(g_health_phase + 1);
    }
  } else {
    g_health_phase = 0;
  }

  diag::tick((uint32_t)(t - g_last_heartbeat_ms));

  // lwIP pcb sample for every connected client, once a second (fw 1.12.0, tcpsnap.h). This is
  // what lets a ws_disconnect record say what TCP thought of the socket — by the time the
  // library reports the disconnect the pcb is already gone. One list walk under the core lock.
  if ((uint32_t)(t - g_last_tcp_sample_ms) >= 1000) {
    g_last_tcp_sample_ms = t;
    for (uint8_t i = 0; i < WEBSOCKETS_SERVER_CLIENT_MAX; i++) {
      const int fd = g_server->fdOf(i);
      if (fd < 0) continue;
      tcpsnap::Snap snap;
      tcpsnap::capture(fd, snap);
      diag::noteTcp(i, snap);
    }
  }

  // Wedge watchdog (see the note by g_at_capacity_since_ms): if every WS slot has been occupied
  // continuously for WEDGE_REBOOT_MS, the server can no longer accept the HA coordinator — reboot
  // to self-heal. Any drop below the cap resets the timer, so only a genuine stuck-full state trips.
  if (g_server->connectedClients() >= WEBSOCKETS_SERVER_CLIENT_MAX) {
    fault::raise(fault::Code::WsCapacity, "all client slots occupied");
    if (g_at_capacity_since_ms == 0) {
      g_at_capacity_since_ms = t;
    } else if ((uint32_t)(t - g_at_capacity_since_ms) >= WEDGE_REBOOT_MS) {
      Serial.printf("[ws] WEDGE: %u clients at cap for >%us — rebooting to self-heal\n",
                    g_server->connectedClients(), WEDGE_REBOOT_MS / 1000);
      Serial.flush();
      delay(50);
      wifi_prov::noteReboot("ws-wedge");
    }
  } else {
    fault::clear(fault::Code::WsCapacity);
    g_at_capacity_since_ms = 0;
  }
}

void notifyStateChanged() { g_dirty = true; }

uint32_t connectedClients() {
  return g_server ? g_server->connectedClients() : 0;
}

uint32_t connectEvents() { return g_conn_events; }
uint32_t disconnectEvents() { return g_disc_events; }
uint32_t errorEvents() { return g_err_events; }

uint32_t skippedWrites() { return g_server ? g_server->skippedWrites() : 0; }

uint32_t deferredReaps() { return g_server ? g_server->deferredReaps() : 0; }

uint32_t heartbeatBroadcasts() { return g_hb_broadcasts; }

uint32_t heartbeatAgeMs() {
  // Unsigned like the gate itself, so a (filtered-out but not impossible) future stamp shows up
  // as an absurd age rather than silently reading as "just fired".
  return (uint32_t)(mono::now() - g_last_heartbeat_ms);
}

}  // namespace ws_api
