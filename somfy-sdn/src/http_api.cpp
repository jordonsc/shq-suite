#include "http_api.h"

#include <Arduino.h>
#include <HTTPUpdate.h>
#include <WebServer.h>
#include <WiFi.h>

#include <cstring>

#include "bus.h"
#include "devices.h"
#include "sdn.h"
#include "version.h"
#include "wifi_prov.h"
#include "ws_api.h"

namespace http_api {

namespace {

WebServer* g_server = nullptr;

const char* modeStr() { return (bus::mode() == bus::Mode::ACTIVE) ? "active" : "listen"; }

int hexNibble(char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  return -1;
}

// Parse a hex byte string (whitespace tolerated) into buf. Returns bytes parsed, or -1.
int parseHexBytes(const String& s, uint8_t* buf, size_t cap) {
  size_t n = 0, i = 0;
  while (i < (size_t)s.length() && n < cap) {
    while (i < (size_t)s.length() && (s[i] == ' ' || s[i] == ':' || s[i] == ',')) i++;
    if (i + 1 >= (size_t)s.length() + 1) break;
    if (i >= (size_t)s.length()) break;
    int h1 = hexNibble(s[i]);
    if (h1 < 0) return -1;
    int h2 = hexNibble(s[i + 1]);
    if (h2 < 0) return -1;
    buf[n++] = (uint8_t)((h1 << 4) | h2);
    i += 2;
  }
  return (int)n;
}

String statusLine() {
  // (build-string bump for OTA verification — set-bottom-limit-by-pulses round)
  devices::DeviceTable& t = bus::table();
  const bus::Stats& st = bus::stats();
  size_t online = 0;
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (d && d->online) online++;
  }
  char buf[420];
  snprintf(buf, sizeof(buf),
           "# mode=%s devices=%u online=%u "
           "tx=%u rx=%u polls=%u "
           "err.cksum=%u err.framing=%u err.timeout=%u err.nack=%u err.total=%u "
           "ws_clients=%u rssi=%d ip=%s fw=\"%s\"",
           modeStr(), (unsigned)t.count(), (unsigned)online,
           st.tx_frames, st.rx_frames, st.polls,
           st.err_checksum, st.err_framing, st.err_timeout, st.err_nack,
           (unsigned)bus::errors().total(),
           (unsigned)ws_api::connectedClients(),
           WiFi.isConnected() ? WiFi.RSSI() : 0,
           WiFi.localIP().toString().c_str(), SOMFY_FW_VERSION " (" __DATE__ " " __TIME__ ")");
  return String(buf);
}

void handleRoot() {
  String b = "Somfy SDN controller\n";
  b += statusLine() + "\n\n";
  b += "GET  /stats                       status line\n";
  b += "GET  /devices                     JSON device table\n";
  b += "GET  /log?since=<seq>&n=<max>      sniffed frames (incremental)\n";
  b += "GET  /errors?n=<max>              error ring (newest first)\n";
  b += "POST /mode?set=listen|active      TX gate (default LISTEN)\n";
  b += "POST /send?addr=AA:BB:CC&msg=0x03&data=04 28 00 00   build+send, report reply\n";
  b += "POST /discover                   discovery sweep (ACTIVE)\n";
  b += "POST /forget?addr=AA:BB:CC        remove a motor from the table\n";
  b += "POST /move?addr=AA:BB:CC&cmd=open|close|stop|pos|jogup|jogdown&value=<ha%|duration>\n";
  b += "POST /wifi?ssid=&password=        set creds, reboot\n";
  b += "POST /update?url=<bin>            HTTP-pull OTA\n";
  b += "POST /clear                      reset ring buffers + counters\n";
  b += "\n# mutating endpoints are POST; curl -X POST (query args still parse)\n";
  g_server->send(200, "text/plain", b);
}

void handleStats() { g_server->send(200, "text/plain", statusLine() + "\n"); }

void handleDevices() {
  devices::DeviceTable& t = bus::table();
  String out = "[";
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (d == nullptr) continue;
    if (i > 0) out += ",";
    char addr[9];
    sdn::formatAddress(d->addr, addr);
    out += "{";
    out += "\"addr\":\"" + String(addr) + "\",";
    out += "\"label\":\"" + String(d->label) + "\",";
    out += "\"position\":" + (d->position_known ? String(d->position_pct) : String("null")) + ",";
    out += "\"pulses\":" + String(d->position_pulses) + ",";
    out += "\"up_limit\":" + String(d->up_limit_pulses) + ",";
    out += "\"down_limit\":" + String(d->down_limit_pulses) + ",";
    out += "\"limits_known\":" + String(d->limits_known ? "true" : "false") + ",";
    out += "\"direction\":\"" + String(d->direction == sdn::DIRECTION_REVERSED ? "reversed" : "normal") + "\",";
    out += "\"moving\":" + String((int)d->movement) + ",";
    out += "\"fault\":" + String(d->fault ? "true" : "false") + ",";
    out += "\"online\":" + String(d->online ? "true" : "false") + ",";
    out += "\"source\":" + String((int)d->source) + ",";
    out += "\"last_seen_ms\":" + String(d->last_seen_ms);
    out += "}";
  }
  out += "]\n";
  g_server->send(200, "application/json", out);
}

void formatSniff(const bus::SniffEntry& e, String& out) {
  char head[48];
  snprintf(head, sizeof(head), "%u %.3f +%uus %s%u:", (unsigned)e.seq, e.t_ms / 1000.0,
           (unsigned)e.gap_us, e.valid ? "" : "?", (unsigned)e.len);
  out += head;
  for (size_t i = 0; i < e.len; i++) {
    char h[4];
    snprintf(h, sizeof(h), " %02X", e.bytes[i]);
    out += h;
  }
  out += " |";
  for (size_t i = 0; i < e.len; i++) {
    char c = (char)e.bytes[i];
    out += (c >= 32 && c < 127) ? c : '.';
  }
  out += "|\n";
}

void handleLog() {
  uint32_t since = g_server->hasArg("since") ? strtoul(g_server->arg("since").c_str(), nullptr, 10) : 0;
  uint32_t limit = g_server->hasArg("n") ? strtoul(g_server->arg("n").c_str(), nullptr, 10) : 1000;
  if (limit > bus::SNIFF_RING) limit = bus::SNIFF_RING;

  static bus::SniffEntry entries[bus::SNIFF_RING];
  uint32_t seq_max = 0;
  size_t n = bus::sniffSince(since, entries, limit, &seq_max);

  String out = "# seq_max=" + String((unsigned)seq_max) + "\n";
  for (size_t i = 0; i < n; i++) formatSniff(entries[i], out);
  g_server->send(200, "text/plain", out);
}

void handleErrors() {
  uint32_t limit = g_server->hasArg("n") ? strtoul(g_server->arg("n").c_str(), nullptr, 10) : 64;
  if (limit > errlog::RING_SIZE) limit = errlog::RING_SIZE;
  static errlog::Entry entries[errlog::RING_SIZE];
  size_t n = bus::errors().newest(entries, limit);
  String out;
  for (size_t i = 0; i < n; i++) {
    errlog::Entry& e = entries[i];
    char line[160];
    if (e.has_addr) {
      char addr[9];
      sdn::formatAddress(e.addr, addr);
      snprintf(line, sizeof(line), "%u %.3f %-11s %s %s", (unsigned)e.seq, e.t_ms / 1000.0,
               errlog::className(e.cls), addr, e.msg);
    } else {
      snprintf(line, sizeof(line), "%u %.3f %-11s -- %s", (unsigned)e.seq, e.t_ms / 1000.0,
               errlog::className(e.cls), e.msg);
    }
    out += line;
    for (size_t k = 0; k < e.hex_len; k++) {
      char h[4];
      snprintf(h, sizeof(h), " %02X", e.hex[k]);
      out += h;
    }
    out += "\n";
  }
  g_server->send(200, "text/plain", out);
}

void handleMode() {
  if (!g_server->hasArg("set")) {
    g_server->send(400, "text/plain", "# need ?set=listen|active\n");
    return;
  }
  String s = g_server->arg("set");
  if (s == "active") bus::setMode(bus::Mode::ACTIVE);
  else if (s == "listen") bus::setMode(bus::Mode::LISTEN);
  else { g_server->send(400, "text/plain", "# set must be listen|active\n"); return; }
  ws_api::notifyStateChanged();
  g_server->send(200, "text/plain", String("# mode -> ") + modeStr() + "\n");
}

void handleSend() {
  if (!g_server->hasArg("msg")) {
    g_server->send(400, "text/plain", "# need ?msg=0xNN (&addr=AA:BB:CC | broadcast) &data=hex\n");
    return;
  }
  uint8_t msg = (uint8_t)strtoul(g_server->arg("msg").c_str(), nullptr, 0);

  uint8_t addr[3];
  bool broadcast = !g_server->hasArg("addr") || g_server->arg("addr") == "broadcast";
  if (!broadcast && !sdn::parseAddress(g_server->arg("addr").c_str(), addr)) {
    g_server->send(400, "text/plain", "# bad addr (want AA:BB:CC)\n");
    return;
  }

  uint8_t data[sdn::MAX_FRAME_LEN];
  int dlen = 0;
  if (g_server->hasArg("data")) {
    dlen = parseHexBytes(g_server->arg("data"), data, sizeof(data));
    if (dlen < 0) { g_server->send(400, "text/plain", "# bad data hex\n"); return; }
  }

  bus::RawResult res;
  // Wait longer than the bus task's worst-case transaction (3 retries x ~1.2 s plus a poll it
  // may queue behind) so we don't give up while the task is still working on our frame.
  bool ok = bus::requestRaw(broadcast ? nullptr : addr, broadcast, msg, data, (size_t)dlen,
                            &res, 8000);
  String out;
  if (!ok) {
    out = "# /send failed (queue/timeout, or bus busy)\n";
  } else if (!res.sent) {
    out = "# NOT SENT: bus is in LISTEN mode. POST /mode?set=active first.\n";
  } else if (!res.got_reply) {
    out = "# sent, no reply (timeout)\n";
  } else {
    out = "# reply: ";
    for (size_t i = 0; i < res.raw_len; i++) {
      char h[4];
      snprintf(h, sizeof(h), "%02X ", res.raw[i]);
      out += h;
    }
    out += "\n# parsed msg_id=0x";
    out += String(res.reply.msg_id, HEX);
    out += " data_len=" + String((unsigned)res.reply.data_len) + " data=";
    for (size_t i = 0; i < res.reply.data_len; i++) {
      char h[4];
      snprintf(h, sizeof(h), "%02X ", res.reply.data[i]);
      out += h;
    }
    out += "\n";
  }
  ws_api::notifyStateChanged();
  g_server->send(200, "text/plain", out);
}

void handleDiscover() {
  if (bus::mode() != bus::Mode::ACTIVE) {
    g_server->send(409, "text/plain", "# REJECTED: set /mode?set=active first\n");
    return;
  }
  bus::Command c;
  c.type = bus::CmdType::REDISCOVER;
  c.broadcast = true;
  c.job_id = bus::nextJobId();
  bus::enqueue(c);
  g_server->send(200, "text/plain", "# discovery sweep queued\n");
}

void handleMove() {
  if (!g_server->hasArg("addr") || !g_server->hasArg("cmd")) {
    g_server->send(400, "text/plain", "# need ?addr=AA:BB:CC&cmd=open|close|stop|pos[&value=<ha%>]\n");
    return;
  }
  uint8_t addr[3];
  if (!sdn::parseAddress(g_server->arg("addr").c_str(), addr)) {
    g_server->send(400, "text/plain", "# bad addr\n");
    return;
  }
  if (bus::mode() != bus::Mode::ACTIVE) {
    g_server->send(409, "text/plain", "# REJECTED: set /mode?set=active first\n");
    return;
  }
  String cmd = g_server->arg("cmd");
  bus::Command c;
  memcpy(c.addr, addr, 3);
  c.job_id = bus::nextJobId();
  if (cmd == "open") c.type = bus::CmdType::OPEN;
  else if (cmd == "close") c.type = bus::CmdType::CLOSE;
  else if (cmd == "stop") c.type = bus::CmdType::STOP;
  else if (cmd == "jogup" || cmd == "jogdown") {
    int dur = g_server->hasArg("value") ? atoi(g_server->arg("value").c_str()) : 20;
    if (dur < sdn::MOVE_DURATION_MIN) dur = sdn::MOVE_DURATION_MIN;
    if (dur > 255) dur = 255;
    c.type = bus::CmdType::MOVE_TIMED;
    c.u8 = (cmd == "jogup") ? 1 : 0;
    c.u16 = (uint16_t)dur;
  }
  else if (cmd == "pos") {
    int ha = g_server->hasArg("value") ? atoi(g_server->arg("value").c_str()) : -1;
    if (ha < 0 || ha > 100) { g_server->send(400, "text/plain", "# pos needs value 0..100 (HA %)\n"); return; }
    c.type = bus::CmdType::SET_POSITION;
    c.u8 = sdn::haToSomfy((uint8_t)ha);
  } else { g_server->send(400, "text/plain", "# cmd must be open|close|stop|pos|jogup|jogdown\n"); return; }
  bus::enqueue(c);
  g_server->send(200, "text/plain", "# queued\n");
}

void handleWifi() {
  if (!g_server->hasArg("ssid")) {
    g_server->send(400, "text/plain", "# need ?ssid=&password=\n");
    return;
  }
  g_server->send(200, "text/plain", "# saving creds, rebooting...\n");
  delay(200);
  wifi_prov::saveCredentials(g_server->arg("ssid").c_str(),
                             g_server->hasArg("password") ? g_server->arg("password").c_str() : "");
}

void handleForget() {
  if (!g_server->hasArg("addr")) {
    g_server->send(400, "text/plain", "# need ?addr=AA:BB:CC\n");
    return;
  }
  uint8_t addr[3];
  if (!sdn::parseAddress(g_server->arg("addr").c_str(), addr)) {
    g_server->send(400, "text/plain", "# bad addr\n");
    return;
  }
  bus::Command c;
  c.type = bus::CmdType::FORGET;
  memcpy(c.addr, addr, 3);
  c.job_id = bus::nextJobId();
  bus::enqueue(c);
  g_server->send(200, "text/plain", "# forget queued\n");
}

void handleClear() {
  bus::clearDiagnostics();
  g_server->send(200, "text/plain", "# cleared\n");
}

void handleUpdate() {
  if (!g_server->hasArg("url")) {
    g_server->send(400, "text/plain", "# need ?url=http://host:port/firmware.bin\n");
    return;
  }
  String url = g_server->arg("url");
  g_server->send(200, "text/plain", "# pulling firmware: " + url + "\n");
  Serial.printf("# OTA: pulling %s\n", url.c_str());

  // Mandatory teardown (SPEC §6.5): suspend the bus task, drain the RX FIFO, force LISTEN —
  // so RS485 traffic can't backlog and starve the download (the Actron OTA-brick fix).
  bus::otaSuspend();

  WiFiClient client;
  httpUpdate.rebootOnUpdate(true);
  t_httpUpdate_return r = httpUpdate.update(client, url);

  // Only reached on failure — success reboots from inside httpUpdate.
  Serial.printf("# OTA failed (%d): %s\n", (int)r, httpUpdate.getLastErrorString().c_str());
  bus::otaResume();
}

}  // namespace

void begin(uint16_t port) {
  g_server = new WebServer(port);
  g_server->on("/", HTTP_GET, handleRoot);
  g_server->on("/stats", HTTP_GET, handleStats);
  g_server->on("/devices", HTTP_GET, handleDevices);
  g_server->on("/log", HTTP_GET, handleLog);
  g_server->on("/errors", HTTP_GET, handleErrors);
  g_server->on("/mode", HTTP_POST, handleMode);
  g_server->on("/send", HTTP_POST, handleSend);
  g_server->on("/discover", HTTP_POST, handleDiscover);
  g_server->on("/move", HTTP_POST, handleMove);
  g_server->on("/forget", HTTP_POST, handleForget);
  g_server->on("/wifi", HTTP_POST, handleWifi);
  g_server->on("/update", HTTP_POST, handleUpdate);
  g_server->on("/clear", HTTP_POST, handleClear);
  g_server->begin();
}

void loop() {
  if (g_server) g_server->handleClient();
}

}  // namespace http_api
