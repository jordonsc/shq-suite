#include "ws_api.h"

#include <Arduino.h>
#include <ArduinoJson.h>
#include <WebSocketsServer.h>

#include <cstring>

#include "bus.h"
#include "devices.h"
#include "sdn.h"

namespace ws_api {

namespace {

WebSocketsServer* g_server = nullptr;
volatile bool g_dirty = false;
uint32_t g_last_heartbeat_ms = 0;

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
  JsonDocument doc;
  buildState(doc);
  String out;
  serializeJson(doc, out);
  g_server->broadcastTXT(out);
}

void sendStateTo(uint8_t client) {
  JsonDocument doc;
  buildState(doc);
  String out;
  serializeJson(doc, out);
  g_server->sendTXT(client, out);
}

void sendAck(uint8_t client, const char* id) {
  JsonDocument doc;
  doc["type"] = "ack";
  if (id) doc["id"] = id;
  doc["status"] = "accepted";
  String out;
  serializeJson(doc, out);
  g_server->sendTXT(client, out);
}

void sendError(uint8_t client, const char* id, const char* msg) {
  JsonDocument doc;
  doc["type"] = "error";
  if (id) doc["id"] = id;
  doc["message"] = msg;
  String out;
  serializeJson(doc, out);
  g_server->sendTXT(client, out);
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

  if (handleMotorCommand(command, obj, &err)) {
    sendAck(client, id);
  } else {
    sendError(client, id, err);
  }
}

void onEvent(uint8_t client, WStype_t type, uint8_t* payload, size_t length) {
  switch (type) {
    case WStype_CONNECTED:
      sendStateTo(client);
      break;
    case WStype_TEXT: {
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
  g_server = new WebSocketsServer(port);
  g_server->begin();
  g_server->onEvent(onEvent);
  g_last_heartbeat_ms = millis();
}

void loop() {
  if (g_server == nullptr) return;
  g_server->loop();

  if (g_dirty) {
    g_dirty = false;
    broadcastState();
    g_last_heartbeat_ms = millis();
  }

  uint32_t t = millis();
  if ((int32_t)(t - g_last_heartbeat_ms) >= (int32_t)HEARTBEAT_INTERVAL_MS) {
    broadcastState();
    g_last_heartbeat_ms = t;
  }
}

void notifyStateChanged() { g_dirty = true; }

uint32_t connectedClients() {
  return g_server ? g_server->connectedClients() : 0;
}

}  // namespace ws_api
