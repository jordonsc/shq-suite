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
#include "ws_guard.h"
#include "version.h"
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
uint32_t g_last_diag_seq_sent = 0;
uint32_t g_last_health_ms = 0;

// Records replayed to a client the moment it connects. A socket can never be told about its own
// death, so this is the ONLY way HA learns why the previous session ended.
constexpr uint32_t DIAG_BACKLOG_MAX = 12;

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

  if (handleMotorCommand(command, obj, &err)) {
    sendAck(client, id);
  } else {
    sendError(client, id, err);
  }
}

// ---- diagnostics fan-out ------------------------------------------------

void sendDiagRecord(const diag::Record& r) {
  if (g_server == nullptr) return;
  JsonDocument doc;
  doc["type"] = "diag";
  JsonObject obj = doc["event"].to<JsonObject>();
  diag::toJson(r, obj);
  String out;
  serializeJson(doc, out);
  g_server->broadcastWritableTXT(out);
}

// Deliberately does NOT advance g_last_diag_seq_sent: a second client would otherwise be starved
// of everything this backlog covered. HA keys on `seq`, so a repeat is discarded there.
void sendDiagBacklog(uint8_t client) {
  if (g_server == nullptr) return;
  const uint32_t last = diag::lastSeq();
  if (last == 0) return;
  uint32_t first = diag::firstSeq();
  if (last - first + 1 > DIAG_BACKLOG_MAX) first = last - DIAG_BACKLOG_MAX + 1;

  JsonDocument doc;
  doc["type"] = "diag_backlog";
  JsonArray arr = doc["events"].to<JsonArray>();
  for (uint32_t seq = first; seq <= last; seq++) {
    const diag::Record* r = diag::bySeq(seq);
    if (r != nullptr) diag::toJson(*r, arr.add<JsonObject>());
  }
  String out;
  serializeJson(doc, out);
  g_server->sendWritableTXT(client, out);
}

void sendHealth() {
  if (g_server == nullptr) return;
  JsonDocument doc;
  doc["type"] = "health";
  JsonObject obj = doc["data"].to<JsonObject>();
  diag::healthToJson(obj);
  // WS-layer counters live here, not in diag, so diag stays independent of the server it watches.
  obj["clients"] = g_server->connectedClients();
  obj["ws_conn"] = g_conn_events;
  obj["ws_disc"] = g_disc_events;
  obj["ws_err"] = g_err_events;
  obj["hb_age_ms"] = (uint32_t)(mono::now() - g_last_heartbeat_ms);
  // Writes the guard declined because the socket would have blocked. Paired with
  // stall_reaps this separates "briefly busy, skipped one frame, recovered" from
  // "socket died" — the gradation an AP that black-holes delivery would paint.
  obj["skipped_writes"] = g_server->skippedWrites();
  obj["deferred_reaps"] = g_server->deferredReaps();
  obj["hb_tx"] = g_hb_broadcasts;
  // Device-level fault (fw 1.10.0, ledger shq-suite-0041). "ok" when clear. This is the signal
  // that was missing when Bed 2's clock wedged for nine hours with every indicator green.
  obj["fault"] = fault::registry().worstSlug();
  obj["fault_detail"] = fault::registry().worstDetail();
  obj["fault_mask"] = fault::registry().mask();
  obj["fw"] = SOMFY_FW_VERSION " " __DATE__ " " __TIME__;
  String out;
  serializeJson(doc, out);
  g_server->broadcastWritableTXT(out);
}

void onEvent(uint8_t client, WStype_t type, uint8_t* payload, size_t length) {
  switch (type) {
    case WStype_CONNECTED: {
      g_conn_events++;
      wifi_prov::noteInbound();  // a completed handshake is inbound traffic (netwatch)
      const IPAddress ip = g_server->remoteIP(client);
      diag::noteWsConnect(client, ip.toString().c_str(), g_server->connectedClients());
      sendStateTo(client);
      // After the snapshot, so a client always has state before it has history.
      sendDiagBacklog(client);
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
  // Protocol-level ping/pong with dead-client eviction. The app-level state heartbeat below is a
  // data push, not a liveness probe — it can't detect a half-open socket. On a marginal WiFi link
  // (e.g. a weak-signal motor) a dropped association leaves the client's TCP connection half-open
  // with no FIN; without this the zombie lingers until lwIP's retransmit timeout (minutes), during
  // which writes to it stall the WS service loop and new connections can't be served. Ping every
  // 15 s, expect a pong within 5 s, disconnect after 2 consecutive misses (~30 s to reap).
  // Parameters live in diag.h so the disconnect classifier reasons about exactly the deadlines
  // that trigger an eviction here (ledger shq-suite-0038).
  g_server->enableHeartbeat(diag::WS_PING_INTERVAL_MS, diag::WS_PONG_TIMEOUT_MS,
                            diag::WS_PONG_MISSES);
  g_last_heartbeat_ms = mono::now();
  g_last_health_ms = g_last_heartbeat_ms;
  g_last_diag_seq_sent = diag::lastSeq();
}

void loop() {
  if (g_server == nullptr) return;
  // Reap BEFORE loop(), not after (fw 1.8.0). enableHeartbeat's ping is sent from inside loop()
  // and writes to the socket DIRECTLY, bypassing the write-guard — so a blocked slot still
  // present when the library runs costs the full ~10 s core write. Dropping it first is what
  // closes the residual 10,016 ms stall seen on this fleet's worst unit (shq-suite-0038).
  g_server->reapStalled(mono::now(), WS_STALL_REAP_MS);

  g_server->loop();

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

  // Drain new diagnostics to any listener. Bounded per iteration so a reconnect burst can't turn
  // one loop pass into the very stall it reports.
  if (g_server->connectedClients() > 0) {
    const uint32_t newest = diag::lastSeq();
    if (g_last_diag_seq_sent < diag::firstSeq()) g_last_diag_seq_sent = diag::firstSeq() - 1;
    uint8_t budget = 4;
    while (g_last_diag_seq_sent < newest && budget-- > 0) {
      const diag::Record* r = diag::bySeq(++g_last_diag_seq_sent);
      if (r != nullptr) sendDiagRecord(*r);
    }
    if ((uint32_t)(t - g_last_health_ms) >= diag::HEALTH_INTERVAL_MS) {
      g_last_health_ms = t;
      sendHealth();
    }
  } else {
    // Nobody listening: keep the cursor at the head so a fresh client gets the backlog once,
    // rather than the backlog and then a replay of the same records.
    g_last_diag_seq_sent = diag::lastSeq();
  }

  diag::tick((uint32_t)(t - g_last_heartbeat_ms));

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
