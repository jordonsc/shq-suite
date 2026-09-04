#include "ws_api.h"

#include <Arduino.h>
#include <ArduinoJson.h>
#include <WebSocketsServer.h>

#include <cstring>

#include "diag.h"
#include "wifi_prov.h"
#include "fault.h"
#include "version.h"
#include "mono.h"
#include "ws_guard.h"

namespace ws_api {

namespace {

// ---- Configuration ------------------------------------------------------

// Pulse length in recognised NEO B-frames for command writes. The MITM-INJECT recipe
// uses n=2 throughout LOCAL-CONTROL-RECIPES.md — two cycles is enough margin for the
// board to see the modified response under transient bus errors.
constexpr size_t COMMAND_PULSE_FRAMES = 2;

// Reg-126 side-channel low-nibble values per LOCAL-CONTROL-RECIPES §4/§6.
constexpr uint16_t REG126_COOL_COMMIT = 0x0001;
constexpr uint16_t REG126_HEAT_COMMIT = 0x0002;

// Address mapping per FINDINGS.md §7.
constexpr uint16_t REG_MODE_WORD = 10;
constexpr uint16_t REG_FAN_WORD = 11;
constexpr uint16_t REG_MAIN_ACTIVE_SP = 12;
constexpr uint16_t REG_COMMAND_CODE = 14;
constexpr uint16_t REG_MAIN_COOL_STORE = 55;
constexpr uint16_t REG_MAIN_HEAT_STORE = 56;
constexpr uint16_t REG_ZONE_ENABLE_MASK = 85;
constexpr uint16_t REG_ZONE_COOL_BASE = 127;
constexpr uint16_t REG_ZONE_HEAT_BASE = 135;
constexpr uint16_t REG_COMMIT_SIGNAL = 126;

// Command codes for reg 14 (per recipes).
constexpr uint16_t CMD_MODE = 0x0001;
constexpr uint16_t CMD_FAN = 0x0002;
constexpr uint16_t CMD_MAIN_SETPOINT = 0x0004;
constexpr uint16_t CMD_ZONE_ENABLE = 0x0040;

// ---- WS server + module state ------------------------------------------

GuardedWebSocketsServer server(8767);
Hooks hooks_{};

state::ControllerState last_published_state_;
bool last_published_valid_ = false;
state::ControllerState current_state_;  // most recent decoder output for transition checks

// Cross-task state handoff (ledger shq-suite-0038, dirty-flag pattern ported from somfy-sdn).
// The priority-5 bridge task decodes NEO frames and hands the result over here; ONLY the main
// loop (ws_api::loop) may touch the WS server, transitions, or the heartbeat stamp. The copy is
// guarded by a spinlock because tickTransitions clears transitions on exact-equality matches
// against setpoints — a torn snapshot could prematurely clear or re-fire a pulse.
portMUX_TYPE pending_state_mux_ = portMUX_INITIALIZER_UNLOCKED;
state::ControllerState pending_state_;  // written under pending_state_mux_ by notifyStateChanged
volatile bool pending_state_dirty_ = false;

uint32_t last_heartbeat_ms_ = 0;

// Wedge watchdog (ported from somfy-sdn ws_api, fw 1.3.0). WebSocketsServer has a hard
// WEBSOCKETS_SERVER_CLIENT_MAX (5) client cap. If every slot fills with live-but-idle zombie
// connections — each kept alive by its own WS keepalive, so enableHeartbeat's reaper never evicts
// them — the library refuses all new handshakes (accept then drop, no HTTP response) and the
// device looks dead to HA while the HTTP API and the RS485 bridge stay perfectly healthy. There
// is only ever ONE legitimate HA coordinator, so sitting at the full cap for minutes is an
// unambiguous wedge: self-heal by rebooting (WiFi creds live in NVS; the bridge task re-inits to
// INJECT on boot, so the A/C keeps bridging). Backstop to the HA client's close-before-reconnect
// fix (actron_mitm_controller 1.1.1). Cannot boot-loop: a fresh boot starts with 0 clients.
constexpr uint32_t WEDGE_REBOOT_MS = 5 * 60 * 1000;  // continuously at cap this long => reboot
uint32_t at_capacity_since_ms_ = 0;                  // millis() when we hit the cap; 0 = below cap

// Lifetime WS event counters (ledger shq-suite-0019 instrumentation) — see ws_api.h.
// Lifetime count of state pushes — paired with hb_age in /stats this answers "is the heartbeat
// still running?" in one curl (ledger shq-suite-0034).
uint32_t hb_broadcasts_ = 0;

uint32_t connect_events_ = 0;
uint32_t disconnect_events_ = 0;
uint32_t error_events_ = 0;

// Diagnostics fan-out cursor (ledger shq-suite-0038). diag records are POLLED from loop() rather
// than pushed from a callback: emitting a WS frame from inside the library's own event dispatch —
// which is where every record originates — would re-enter the server mid-iteration.
uint32_t last_diag_seq_sent_ = 0;
uint32_t last_health_ms_ = 0;

// Records replayed to a client the moment it connects. A socket can never be told about its own
// death, so the ONLY way HA learns why the previous session ended is to be handed the backlog on
// the next one. Capped because the whole batch is serialised into one heap-allocated frame.
constexpr uint32_t DIAG_BACKLOG_MAX = 12;

// ---- Transition table --------------------------------------------------
//
// Pulse-style transitions (mode / fan / master setpoint / zone enable) don't need rule
// cleanup — the bridge auto-expires the pulse after COMMAND_PULSE_FRAMES. We only track
// the target value + deadline so we can publish `_transitioning` and clear when the
// board adopts the change.
//
// Zone setpoint transitions are persistent INJECT rules and DO need explicit cleanup —
// they live in zsp_t_ and the rule set is recomputed each tick. In AUTO mode the
// transition runs two phases: write cool array first, then heat array.

// Max rules a single pulse needs: master setpoint in AUTO writes active SP + cool store +
// heat store + command code = 4. Everything else uses 2.
constexpr size_t PULSE_MAX_RULES = 4;

struct PulseTransition {
  bool active = false;
  uint16_t target_raw = 0;          // mode word / fan word / setpoint raw / mask
  uint32_t last_fire_ms = 0;        // when we last (re-)fired the pulse
  uint8_t attempts = 0;             // fires so far (initial + retries), capped at PULSE_MAX_FIRES
  bridge::Rule rules[PULSE_MAX_RULES];  // stored so tickTransitions can re-fire on a miss
  size_t n_rules = 0;
};

struct ZoneSetpointTransition {
  bool active = false;
  uint16_t target_raw = 0;
  uint32_t deadline_ms = 0;
  uint8_t phase = 0;        // 0=cool array commit, 1=heat array commit
  bool needs_both = false;  // AUTO mode → run both phases
};

PulseTransition mode_t_;
PulseTransition fan_t_;
PulseTransition msp_t_;
PulseTransition ze_t_;
ZoneSetpointTransition zsp_t_[state::NUM_ZONES];

// ---- Helpers ------------------------------------------------------------

// Filtered clock (ledger shq-suite-0034): a raw millis() here occasionally returns a far-future
// value, and any deadline stamped from one stops firing until the real clock catches up.
uint32_t now_ms() { return mono::now(); }

// Arm (or replace) a pulse transition: store the rules so we can re-fire on a miss, fire the
// first pulse now, and start the retry/attempt accounting. Latest-wins — calling this again
// for the same field replaces the target, rules, and attempt count.
void armPulse(PulseTransition& tr, uint16_t target_raw, const bridge::Rule* rules, size_t n) {
  tr.active = true;
  tr.target_raw = target_raw;
  tr.n_rules = (n > PULSE_MAX_RULES) ? PULSE_MAX_RULES : n;
  for (size_t i = 0; i < tr.n_rules; i++) tr.rules[i] = rules[i];
  tr.attempts = 1;
  tr.last_fire_ms = now_ms();
  if (hooks_.injector != nullptr) hooks_.injector->setPulse(tr.rules, tr.n_rules, COMMAND_PULSE_FRAMES);
}

// Ensure the bridge is in INJECT mode before issuing a write. Plan §"Bridge mode" — we
// keep it permanently in INJECT; this is a safety guard in case an operator flipped it
// to PASSTHRU manually.
bool ensureInject() {
  if (hooks_.mode_var == nullptr || hooks_.set_mode == nullptr) return false;
  auto mode = *hooks_.mode_var;
  if (mode == bridge::StreamingBridge::OFF || mode == bridge::StreamingBridge::RESPOND) {
    return false;  // operator-driven modes; we don't override
  }
  if (mode != bridge::StreamingBridge::INJECT) {
    hooks_.set_mode(bridge::StreamingBridge::INJECT);
  }
  return true;
}

// Recompute and push the full set of persistent INJECT rules to the bridge based on the
// active zone-setpoint transitions. Pulse rules (mode/fan/etc.) are not managed here —
// they're set ad hoc by handleCommand and auto-expire in the bridge.
void rebuildPersistentRules() {
  if (hooks_.injector == nullptr) return;

  // Two-phase commit: process cool-phase transitions first. If any cool-phase
  // transitions exist, they own reg 126 = 0x0001. Otherwise heat-phase transitions
  // own reg 126 = 0x0002. If neither, clear all persistent rules.
  bool has_cool_phase = false;
  bool has_heat_phase = false;
  for (size_t i = 0; i < state::NUM_ZONES; i++) {
    if (!zsp_t_[i].active) continue;
    if (zsp_t_[i].phase == 0) has_cool_phase = true;
    else                       has_heat_phase = true;
  }

  bridge::Rule rules[bridge::StreamingBridge::MAX_RULES];
  size_t n = 0;
  if (has_cool_phase) {
    for (size_t i = 0; i < state::NUM_ZONES && n < bridge::StreamingBridge::MAX_RULES - 1; i++) {
      if (zsp_t_[i].active && zsp_t_[i].phase == 0) {
        rules[n++] = {(uint16_t)(REG_ZONE_COOL_BASE + i), zsp_t_[i].target_raw};
      }
    }
    rules[n++] = {REG_COMMIT_SIGNAL, REG126_COOL_COMMIT};
  } else if (has_heat_phase) {
    for (size_t i = 0; i < state::NUM_ZONES && n < bridge::StreamingBridge::MAX_RULES - 1; i++) {
      if (zsp_t_[i].active && zsp_t_[i].phase == 1) {
        rules[n++] = {(uint16_t)(REG_ZONE_HEAT_BASE + i), zsp_t_[i].target_raw};
      }
    }
    rules[n++] = {REG_COMMIT_SIGNAL, REG126_HEAT_COMMIT};
  }
  hooks_.injector->setRules(rules, n);
}

// ---- JSON: state payload ------------------------------------------------

// Returns null when raw == SENTINEL_NONE; otherwise raw/10 as a float.
void addTemp(JsonObject obj, const char* key, uint16_t raw) {
  if (raw == state::SENTINEL_NONE) obj[key] = nullptr;
  else                              obj[key] = raw / 10.0;
}

void addOptStr(JsonObject obj, const char* key, const char* str_or_null) {
  if (str_or_null == nullptr) obj[key] = nullptr;
  else                        obj[key] = str_or_null;
}

// Active master setpoint as float °C, or null when not applicable.
void addMasterSetpoint(JsonObject obj, const char* key, const state::ControllerState& s) {
  uint16_t raw = state::activeMasterSetpoint(s);
  addTemp(obj, key, raw);
}

void buildStatePayload(JsonDocument& doc, const state::ControllerState& s) {
  doc["type"] = "state";
  JsonObject data = doc["data"].to<JsonObject>();

  // Master mode
  if (s.mode == state::Mode::Unknown) data["mode"] = nullptr;
  else                                data["mode"] = state::modeName(s.mode);
  if (mode_t_.active) {
    data["mode_transitioning"] = state::modeName((state::Mode)(mode_t_.target_raw & 0x07));
  } else {
    data["mode_transitioning"] = nullptr;
  }

  // Master fan
  if (s.fan == state::FanSpeed::Unknown) data["fan"] = nullptr;
  else                                   data["fan"] = state::fanName(s.fan);
  if (fan_t_.active) {
    data["fan_transitioning"] = state::fanName((state::FanSpeed)(fan_t_.target_raw & 0x0F));
  } else {
    data["fan_transitioning"] = nullptr;
  }

  // Master setpoint
  addMasterSetpoint(data, "master_setpoint", s);
  if (msp_t_.active) data["master_setpoint_transitioning"] = msp_t_.target_raw / 10.0;
  else               data["master_setpoint_transitioning"] = nullptr;

  // Master current temp — indoor-unit main/return-air reading (reg 13). Read-only, no
  // transition counterpart (not a controllable field).
  addTemp(data, "current_temp", s.main_temp_raw);

  // Zones
  JsonArray zones = data["zones"].to<JsonArray>();
  for (size_t i = 0; i < state::NUM_ZONES; i++) {
    JsonObject z = zones.add<JsonObject>();
    z["id"] = (int)i;
    z["enabled"] = s.zones[i].enabled;
    if (ze_t_.active) {
      bool target_bit = (ze_t_.target_raw & (1u << i)) != 0;
      // Only publish a per-zone enabled_transitioning when the target differs from current.
      if (target_bit != s.zones[i].enabled) z["enabled_transitioning"] = target_bit;
      else                                  z["enabled_transitioning"] = nullptr;
    } else {
      z["enabled_transitioning"] = nullptr;
    }
    addTemp(z, "current_temp", s.zones[i].current_temp_raw);

    uint16_t target = state::activeZoneSetpoint(s, i);
    addTemp(z, "target_temp", target);
    if (zsp_t_[i].active) z["target_temp_transitioning"] = zsp_t_[i].target_raw / 10.0;
    else                  z["target_temp_transitioning"] = nullptr;
  }
}

void sendStateToClient(uint8_t client_id, const state::ControllerState& s) {
  JsonDocument doc;
  buildStatePayload(doc, s);
  String out;
  serializeJson(doc, out);
  server.sendWritableTXT(client_id, out);
}

void broadcastState(const state::ControllerState& s) {
  hb_broadcasts_++;
  diag::noteWsTx();
  JsonDocument doc;
  buildStatePayload(doc, s);
  String out;
  serializeJson(doc, out);
  server.broadcastWritableTXT(out);
}

// ---- diagnostics fan-out ------------------------------------------------

void sendDiagRecord(const diag::Record& r) {
  JsonDocument doc;
  doc["type"] = "diag";
  JsonObject obj = doc["event"].to<JsonObject>();
  diag::toJson(r, obj);
  String out;
  serializeJson(doc, out);
  server.broadcastWritableTXT(out);
}

// Deliberately does NOT advance last_diag_seq_sent_: a second client would otherwise be starved of
// everything this backlog covered. HA keys on `seq`, so a record arriving twice is discarded there.
void sendDiagBacklog(uint8_t client_id) {
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
  server.sendWritableTXT(client_id, out);
}

void sendHealth() {
  JsonDocument doc;
  doc["type"] = "health";
  JsonObject obj = doc["data"].to<JsonObject>();
  diag::healthToJson(obj);
  // WS-layer counters live here, not in diag, so diag stays independent of the server it watches.
  obj["clients"] = server.connectedClients();
  obj["ws_conn"] = connect_events_;
  obj["ws_disc"] = disconnect_events_;
  obj["ws_err"] = error_events_;
  obj["hb_age_ms"] = (uint32_t)(mono::now() - last_heartbeat_ms_);
  // Writes the guard declined because the socket would have blocked. Paired with
  // stall_reaps this separates "briefly busy, skipped one frame, recovered" from
  // "socket died" — the gradation an AP that black-holes delivery would paint.
  obj["skipped_writes"] = server.skippedWrites();
  obj["deferred_reaps"] = server.deferredReaps();
  obj["hb_tx"] = hb_broadcasts_;
  // Device-level fault (fw 1.10.0, ledger shq-suite-0041). "ok" when clear. This is the signal
  // that was missing when the somfy twin's clock wedged for nine hours with every indicator green.
  obj["fault"] = fault::registry().worstSlug();
  obj["fault_detail"] = fault::registry().worstDetail();
  obj["fault_mask"] = fault::registry().mask();
  obj["fw"] = ACTRON_FW_VERSION " " __DATE__ " " __TIME__;
  String out;
  serializeJson(doc, out);
  server.broadcastWritableTXT(out);
}

void sendAck(uint8_t client_id, const char* id) {
  JsonDocument doc;
  doc["type"] = "ack";
  if (id != nullptr) doc["id"] = id;
  doc["status"] = "accepted";
  String out;
  serializeJson(doc, out);
  server.sendWritableTXT(client_id, out);
}

void sendError(uint8_t client_id, const char* id, const char* message) {
  JsonDocument doc;
  doc["type"] = "error";
  if (id != nullptr) doc["id"] = id;
  doc["message"] = message;
  String out;
  serializeJson(doc, out);
  server.sendWritableTXT(client_id, out);
}

// ---- Command handlers --------------------------------------------------

// Build the reg-10 mode word: preserve feature bits from current state, replace mode bits.
uint16_t buildModeWord(state::Mode m) {
  uint16_t base = current_state_.mode_word_raw;
  uint16_t cleared = (uint16_t)(base & ~0x0007);
  cleared |= 0x8000;  // always-set bit per FINDINGS §7
  return (uint16_t)(cleared | ((uint16_t)m & 0x07));
}

// Build the reg-11 fan word: preserve high byte + cont-fan bit (bit 7), replace speed nibble.
uint16_t buildFanWord(state::FanSpeed f) {
  uint16_t base = current_state_.fan_word_raw;
  uint16_t cleared = (uint16_t)(base & ~0x000F);
  return (uint16_t)(cleared | ((uint16_t)f & 0x0F));
}

bool cmdSetMode(const JsonObject& cmd, const char** error_out) {
  const char* v = cmd["value"];
  state::Mode m = state::parseMode(v);
  if (m == state::Mode::Unknown) { *error_out = "invalid mode"; return false; }
  if (!ensureInject()) { *error_out = "bridge mode incompatible (off/respond)"; return false; }

  uint16_t word = buildModeWord(m);
  bridge::Rule rules[2] = {{REG_MODE_WORD, word}, {REG_COMMAND_CODE, CMD_MODE}};
  armPulse(mode_t_, word, rules, 2);
  return true;
}

bool cmdSetFan(const JsonObject& cmd, const char** error_out) {
  const char* v = cmd["value"];
  state::FanSpeed f = state::parseFan(v);
  if (f == state::FanSpeed::Unknown) { *error_out = "invalid fan speed"; return false; }
  if (!ensureInject()) { *error_out = "bridge mode incompatible (off/respond)"; return false; }

  uint16_t word = buildFanWord(f);
  bridge::Rule rules[2] = {{REG_FAN_WORD, word}, {REG_COMMAND_CODE, CMD_FAN}};
  armPulse(fan_t_, word, rules, 2);
  return true;
}

bool cmdSetMasterSetpoint(const JsonObject& cmd, const char** error_out) {
  if (!cmd["value"].is<float>() && !cmd["value"].is<int>()) {
    *error_out = "value must be number"; return false;
  }
  float v = cmd["value"].as<float>();
  if (v < 10.0 || v > 35.0) { *error_out = "value out of range (10..35)"; return false; }
  if (current_state_.mode == state::Mode::Off || current_state_.mode == state::Mode::Fan ||
      current_state_.mode == state::Mode::Unknown) {
    *error_out = "master setpoint not applicable in current mode"; return false;
  }
  if (!ensureInject()) { *error_out = "bridge mode incompatible (off/respond)"; return false; }

  uint16_t raw = (uint16_t)(v * 10.0f + 0.5f);

  // Per recipe §3: rules = {12:val, <store>:val, 14:0x04}. In AUTO write both stores so
  // we don't desync them on the next mode toggle.
  bridge::Rule rules[4];
  size_t n = 0;
  rules[n++] = {REG_MAIN_ACTIVE_SP, raw};
  if (current_state_.mode == state::Mode::Cool) {
    rules[n++] = {REG_MAIN_COOL_STORE, raw};
  } else if (current_state_.mode == state::Mode::Heat) {
    rules[n++] = {REG_MAIN_HEAT_STORE, raw};
  } else {  // Auto
    rules[n++] = {REG_MAIN_COOL_STORE, raw};
    rules[n++] = {REG_MAIN_HEAT_STORE, raw};
  }
  rules[n++] = {REG_COMMAND_CODE, CMD_MAIN_SETPOINT};
  armPulse(msp_t_, raw, rules, n);
  return true;
}

bool cmdSetZoneEnabled(const JsonObject& cmd, const char** error_out) {
  if (!cmd["zone"].is<int>()) { *error_out = "missing zone"; return false; }
  int z = cmd["zone"].as<int>();
  if (z < 0 || z >= (int)state::NUM_ZONES) { *error_out = "zone out of range"; return false; }
  if (!cmd["value"].is<bool>()) { *error_out = "value must be bool"; return false; }
  bool target = cmd["value"].as<bool>();
  if (!ensureInject()) { *error_out = "bridge mode incompatible (off/respond)"; return false; }

  // Base on the pending transition target if there is one, so concurrent zone-enable
  // commands layer correctly. Without this, the bus state is stale during a transition
  // and the second command silently drops the first command's bit change.
  uint8_t mask = ze_t_.active ? (uint8_t)(ze_t_.target_raw & 0xFF)
                              : current_state_.zone_enable_mask;
  if (target) mask = (uint8_t)(mask | (1u << z));
  else        mask = (uint8_t)(mask & ~(1u << z));

  bridge::Rule rules[2] = {{REG_ZONE_ENABLE_MASK, mask}, {REG_COMMAND_CODE, CMD_ZONE_ENABLE}};
  armPulse(ze_t_, mask, rules, 2);
  return true;
}

bool cmdSetZoneSetpoint(const JsonObject& cmd, const char** error_out) {
  if (!cmd["zone"].is<int>()) { *error_out = "missing zone"; return false; }
  int z = cmd["zone"].as<int>();
  if (z < 0 || z >= (int)state::NUM_ZONES) { *error_out = "zone out of range"; return false; }
  if (!cmd["value"].is<float>() && !cmd["value"].is<int>()) {
    *error_out = "value must be number"; return false;
  }
  float v = cmd["value"].as<float>();
  if (v < 10.0 || v > 35.0) { *error_out = "value out of range (10..35)"; return false; }
  if (current_state_.mode == state::Mode::Off || current_state_.mode == state::Mode::Fan ||
      current_state_.mode == state::Mode::Unknown) {
    *error_out = "zone setpoint not applicable in current mode"; return false;
  }
  if (!ensureInject()) { *error_out = "bridge mode incompatible (off/respond)"; return false; }

  uint16_t raw = (uint16_t)(v * 10.0f + 0.5f);

  // Phase 0 = cool array commit. In COOL or AUTO mode we start here. In HEAT we skip
  // straight to phase 1 (heat array commit, the only one that matters).
  ZoneSetpointTransition& t = zsp_t_[z];
  t.active = true;
  t.target_raw = raw;
  t.needs_both = (current_state_.mode == state::Mode::Auto);
  if (current_state_.mode == state::Mode::Heat) t.phase = 1;
  else                                          t.phase = 0;
  t.deadline_ms = now_ms() + ZONE_SETPOINT_GRACE_MS;

  rebuildPersistentRules();
  return true;
}

void handleCommand(uint8_t client_id, JsonDocument& cmd) {
  const char* type = cmd["type"];
  if (type == nullptr || strcmp(type, "command") != 0) {
    sendError(client_id, cmd["id"], "expected type=command");
    return;
  }
  const char* command = cmd["command"];
  if (command == nullptr) {
    sendError(client_id, cmd["id"], "missing command");
    return;
  }

  if (strcmp(command, "reboot") == 0) {
    // Ack first so HA sees the command land — the reply has to clear the socket before the stack
    // goes down.
    //
    // NOTE THE COST, which is real and specific to this firmware: this bridge sits between a
    // PHYSICALLY CUT RS485 bus, so while the ESP32 is down the NEO<->indoor-board link is SEVERED,
    // not passed through. A reboot is ~8-30 s during which the A/C has no control bus at all.
    // Prefer to reboot with the zones off. (The somfy twin has no such cost — it is an ordinary
    // bus participant and the motors simply stop being polled.)
    sendAck(client_id, cmd["id"]);
    Serial.println("# reboot requested (ws-command)");
    Serial.flush();
    delay(100);
    ESP.restart();
    return;
  }

  JsonObject obj = cmd.as<JsonObject>();
  const char* error_msg = "unknown command";
  bool ok = false;
  if      (strcmp(command, "set_mode") == 0)              ok = cmdSetMode(obj, &error_msg);
  else if (strcmp(command, "set_fan") == 0)               ok = cmdSetFan(obj, &error_msg);
  else if (strcmp(command, "set_master_setpoint") == 0)   ok = cmdSetMasterSetpoint(obj, &error_msg);
  else if (strcmp(command, "set_zone_enabled") == 0)      ok = cmdSetZoneEnabled(obj, &error_msg);
  else if (strcmp(command, "set_zone_setpoint") == 0)     ok = cmdSetZoneSetpoint(obj, &error_msg);

  if (ok) {
    sendAck(client_id, cmd["id"]);
    // Push a state update immediately so the client sees the `_transitioning` value.
    broadcastState(current_state_);
    // Update last_published so we don't immediately push again on next decoder tick.
    last_published_state_ = current_state_;
    last_published_valid_ = true;
  } else {
    sendError(client_id, cmd["id"], error_msg);
  }
}

// ---- Transition ticks ---------------------------------------------------

// Did the captured state catch up to the target? When it has, clear the transition.
// Persistent rules (zone setpoints) also need rule cleanup via rebuildPersistentRules.
bool didStateAdoptModeTarget() {
  if (!mode_t_.active) return false;
  return ((uint16_t)current_state_.mode_word_raw & 0x07) == (mode_t_.target_raw & 0x07);
}

bool didStateAdoptFanTarget() {
  if (!fan_t_.active) return false;
  return ((uint16_t)current_state_.fan_word_raw & 0x0F) == (fan_t_.target_raw & 0x0F);
}

bool didStateAdoptMspTarget() {
  if (!msp_t_.active) return false;
  // In COOL/HEAT we check the active store; in AUTO check both.
  switch (current_state_.mode) {
    case state::Mode::Cool: return current_state_.cool_main_setpoint_raw == msp_t_.target_raw;
    case state::Mode::Heat: return current_state_.heat_main_setpoint_raw == msp_t_.target_raw;
    case state::Mode::Auto:
      return current_state_.cool_main_setpoint_raw == msp_t_.target_raw &&
             current_state_.heat_main_setpoint_raw == msp_t_.target_raw;
    default: return false;
  }
}

bool didStateAdoptZeTarget() {
  if (!ze_t_.active) return false;
  return current_state_.zone_enable_mask == (uint8_t)(ze_t_.target_raw & 0xFF);
}

// Advance one pulse transition: clear it on adoption, otherwise re-fire the stored pulse once
// per PULSE_RETRY_INTERVAL_MS until PULSE_MAX_FIRES is exhausted, then give up.
void tickPulse(PulseTransition& tr, bool adopted, uint32_t t) {
  if (!tr.active) return;
  if (adopted) { tr.active = false; return; }
  if ((uint32_t)(t - tr.last_fire_ms) < PULSE_RETRY_INTERVAL_MS) return;
  if (tr.attempts >= PULSE_MAX_FIRES) {
    tr.active = false;  // exhausted all attempts and the last window elapsed — write is lost
    return;
  }
  if (hooks_.injector != nullptr) hooks_.injector->setPulse(tr.rules, tr.n_rules, COMMAND_PULSE_FRAMES);
  tr.last_fire_ms = t;
  tr.attempts++;
}

void tickTransitions() {
  uint32_t t = now_ms();
  bool persistent_rules_dirty = false;

  tickPulse(mode_t_, didStateAdoptModeTarget(), t);
  tickPulse(fan_t_, didStateAdoptFanTarget(), t);
  tickPulse(msp_t_, didStateAdoptMspTarget(), t);
  tickPulse(ze_t_, didStateAdoptZeTarget(), t);

  for (size_t i = 0; i < state::NUM_ZONES; i++) {
    auto& tr = zsp_t_[i];
    if (!tr.active) continue;
    if ((int32_t)(t - tr.deadline_ms) >= 0) {
      // Timed out — bail entirely (drop both phases). Surface as "no transition" so HA
      // falls back to the actual board value.
      tr.active = false;
      persistent_rules_dirty = true;
      continue;
    }
    if (tr.phase == 0) {
      // Cool array commit — match against cool store
      if (current_state_.zones[i].cool_setpoint_raw == tr.target_raw) {
        if (tr.needs_both) {
          tr.phase = 1;
          tr.deadline_ms = t + ZONE_SETPOINT_GRACE_MS;  // fresh budget for heat phase
        } else {
          tr.active = false;
        }
        persistent_rules_dirty = true;
      }
    } else {
      // Heat array commit
      if (current_state_.zones[i].heat_setpoint_raw == tr.target_raw) {
        tr.active = false;
        persistent_rules_dirty = true;
      }
    }
  }

  if (persistent_rules_dirty) rebuildPersistentRules();
}

// ---- WS callbacks ------------------------------------------------------

void onEvent(uint8_t client_id, WStype_t type, uint8_t* payload, size_t length) {
  switch (type) {
    case WStype_CONNECTED: {
      connect_events_++;
      wifi_prov::noteInbound();  // a completed handshake is inbound traffic (netwatch)
      const IPAddress ip = server.remoteIP(client_id);
      diag::noteWsConnect(client_id, ip.toString().c_str(), server.connectedClients());
      sendStateToClient(client_id, current_state_);
      // After the snapshot, so a client always has state before it has history.
      sendDiagBacklog(client_id);
      break;
    }
    case WStype_DISCONNECTED:
      disconnect_events_++;
      diag::noteWsDisconnect(client_id, server.connectedClients());
      break;
    case WStype_ERROR:
      error_events_++;
      diag::noteWsError(client_id, server.connectedClients());
      break;
    case WStype_PONG:
      // Proof the peer answered our reaper's ping. Its absence is what gets a client evicted, so
      // the age of the last one is the primary evidence in a disconnect classification.
      diag::noteWsPong(client_id);
      wifi_prov::noteInbound();
      break;
    case WStype_PING:
      diag::noteWsRx(client_id);
      wifi_prov::noteInbound();
      break;
    case WStype_TEXT: {
      diag::noteWsRx(client_id);
      wifi_prov::noteInbound();
      JsonDocument doc;
      DeserializationError err = deserializeJson(doc, payload, length);
      if (err) {
        sendError(client_id, nullptr, "invalid JSON");
        return;
      }
      handleCommand(client_id, doc);
      break;
    }
    default:
      break;  // BIN and friends: no-op
  }
}

// ---- State diff --------------------------------------------------------

bool stateChanged(const state::ControllerState& a, const state::ControllerState& b) {
  if (a.mode != b.mode) return true;
  if (a.fan != b.fan) return true;
  if (a.continuous_fan != b.continuous_fan) return true;
  if (a.mode_word_raw != b.mode_word_raw) return true;
  if (a.fan_word_raw != b.fan_word_raw) return true;
  if (a.main_setpoint_raw != b.main_setpoint_raw) return true;
  if (a.cool_main_setpoint_raw != b.cool_main_setpoint_raw) return true;
  if (a.heat_main_setpoint_raw != b.heat_main_setpoint_raw) return true;
  if (a.zone_enable_mask != b.zone_enable_mask) return true;
  for (size_t i = 0; i < state::NUM_ZONES; i++) {
    if (a.zones[i].enabled != b.zones[i].enabled) return true;
    if (a.zones[i].current_temp_raw != b.zones[i].current_temp_raw) return true;
    if (a.zones[i].cool_setpoint_raw != b.zones[i].cool_setpoint_raw) return true;
    if (a.zones[i].heat_setpoint_raw != b.zones[i].heat_setpoint_raw) return true;
  }
  return false;
}

}  // namespace

// ---- Public API --------------------------------------------------------

void begin(uint16_t port, const Hooks& hooks) {
  hooks_ = hooks;
  // Re-bind server to requested port (constructor uses default 8767 — if the caller
  // wants a different one, recreate via placement new). Simpler: ignore `port` if it
  // matches the constructor default. The harness only uses 8767.
  (void)port;
  server.begin();
  server.onEvent(onEvent);
  // Protocol-level ping/pong with dead-client eviction (ported from somfy-sdn ws_api, fw 1.1.5).
  // The app-level state heartbeat below is a data push, not a liveness probe — it can't detect a
  // half-open socket. When a brief WiFi disruption drops the HA client's association without a TCP
  // FIN, the zombie connection lingers until lwIP's retransmit timeout (minutes), during which
  // writes to it stall the WS service loop and HA sees no state → `unavailable`. Ping every 15 s,
  // expect a pong within 5 s, disconnect after 2 consecutive misses (~30 s to reap). This is the
  // primary fix for the recurring 30-40 s `unavailable` flaps (ledger shq-suite-0019).
  // Parameters live in diag.h so the disconnect classifier reasons about exactly the deadlines
  // that trigger an eviction here (ledger shq-suite-0038).
  server.enableHeartbeat(diag::WS_PING_INTERVAL_MS, diag::WS_PONG_TIMEOUT_MS, diag::WS_PONG_MISSES);
  last_heartbeat_ms_ = mono::now();
  last_health_ms_ = last_heartbeat_ms_;
  last_diag_seq_sent_ = diag::lastSeq();
}

void loop() {
  // Reap BEFORE server.loop(), not after (fw 1.8.0). enableHeartbeat's ping is sent from inside
  // server.loop() and writes to the socket DIRECTLY, bypassing the write-guard — so a blocked
  // slot that is still present when the library runs costs the full ~10 s core write. Dropping
  // it first is what closes the residual 10,016 ms stall seen on somfy_sdn_06 (shq-suite-0038).
  server.reapStalled(mono::now(), WS_STALL_REAP_MS);

  server.loop();

  // Drain the bridge task's state handoff. Dirty is cleared inside the critical section BEFORE
  // the broadcast (same ordering as somfy-sdn): a bus update landing mid-broadcast re-sets the
  // flag and gets published next pass instead of being lost.
  bool fresh_state = false;
  if (pending_state_dirty_) {
    portENTER_CRITICAL(&pending_state_mux_);
    current_state_ = pending_state_;
    pending_state_dirty_ = false;
    portEXIT_CRITICAL(&pending_state_mux_);
    fresh_state = true;
  }

  // Tick AFTER the copy so the published payload reflects transitions that just completed on
  // this frame (previously done inside publishStateIfChanged on the bridge task).
  tickTransitions();

  if (fresh_state && (!last_published_valid_ || stateChanged(current_state_, last_published_state_))) {
    broadcastState(current_state_);
    last_published_state_ = current_state_;
    last_published_valid_ = true;
    last_heartbeat_ms_ = mono::now();
  }

  uint32_t t = mono::now();

  // Drain new diagnostics to any listener. Bounded per iteration so a burst (a reconnect storm
  // produces several records at once) can't turn one loop pass into the very stall it reports.
  if (server.connectedClients() > 0) {
    const uint32_t newest = diag::lastSeq();
    if (last_diag_seq_sent_ < diag::firstSeq()) last_diag_seq_sent_ = diag::firstSeq() - 1;
    uint8_t budget = 4;
    while (last_diag_seq_sent_ < newest && budget-- > 0) {
      const diag::Record* r = diag::bySeq(++last_diag_seq_sent_);
      if (r != nullptr) sendDiagRecord(*r);
    }
    if ((uint32_t)(t - last_health_ms_) >= diag::HEALTH_INTERVAL_MS) {
      last_health_ms_ = t;
      sendHealth();
    }
  } else {
    // Nobody listening: keep the cursor at the head so a fresh client gets the backlog once,
    // rather than the backlog and then a replay of the same records.
    last_diag_seq_sent_ = diag::lastSeq();
  }

  diag::tick((uint32_t)(t - last_heartbeat_ms_));
  // UNSIGNED elapsed-time test, deliberately (ledger shq-suite-0034). The signed form read as
  // "not due yet" whenever the stamp sat in the future, and a single far-future millis() read put
  // it there — after which this device accepts connections and answers WS pings but never pushes
  // state, which is exactly the "silent socket" HA has been flapping on (shq-suite-0019).
  // Unsigned subtraction wraps a future stamp to a huge elapsed value, so the next loop fires and
  // re-stamps it. mono::now() is the second layer: it filters the bad reads out up front.
  if ((uint32_t)(t - last_heartbeat_ms_) >= HEARTBEAT_INTERVAL_MS) {
    broadcastState(current_state_);
    last_published_state_ = current_state_;
    last_published_valid_ = true;
    last_heartbeat_ms_ = t;
  }

  // Wedge watchdog (see the note by at_capacity_since_ms_): if every WS slot has been occupied
  // continuously for WEDGE_REBOOT_MS, the server can no longer accept the HA coordinator — reboot
  // to self-heal. Any drop below the cap resets the timer, so only a genuine stuck-full state trips.
  if (server.connectedClients() >= WEBSOCKETS_SERVER_CLIENT_MAX) {
    fault::raise(fault::Code::WsCapacity, "all client slots occupied");
    if (at_capacity_since_ms_ == 0) {
      at_capacity_since_ms_ = t;
    } else if ((uint32_t)(t - at_capacity_since_ms_) >= WEDGE_REBOOT_MS) {
      Serial.printf("[ws] WEDGE: %u clients at cap for >%us — rebooting to self-heal\n",
                    (unsigned)server.connectedClients(), WEDGE_REBOOT_MS / 1000);
      Serial.flush();
      delay(50);
      ESP.restart();
    }
  } else {
    fault::clear(fault::Code::WsCapacity);
    at_capacity_since_ms_ = 0;
  }
}

void notifyStateChanged(const state::ControllerState& s) {
  // Bridge-task side of the handoff: a ~100-byte struct copy under a spinlock, nothing else.
  // Microseconds — three orders of magnitude under the RS485 t3.5 frame-gap budget.
  portENTER_CRITICAL(&pending_state_mux_);
  pending_state_ = s;
  pending_state_dirty_ = true;
  portEXIT_CRITICAL(&pending_state_mux_);
}

size_t connectedClients() { return server.connectedClients(); }

uint32_t connectEvents() { return connect_events_; }
uint32_t disconnectEvents() { return disconnect_events_; }
uint32_t errorEvents() { return error_events_; }

uint32_t skippedWrites() { return server.skippedWrites(); }

uint32_t deferredReaps() { return server.deferredReaps(); }

uint32_t heartbeatBroadcasts() { return hb_broadcasts_; }

uint32_t heartbeatAgeMs() { return (uint32_t)(mono::now() - last_heartbeat_ms_); }

size_t pendingTransitions() {
  size_t n = (mode_t_.active ? 1 : 0) + (fan_t_.active ? 1 : 0) +
             (msp_t_.active ? 1 : 0) + (ze_t_.active ? 1 : 0);
  for (size_t i = 0; i < state::NUM_ZONES; i++) if (zsp_t_[i].active) n++;
  return n;
}

}  // namespace ws_api
