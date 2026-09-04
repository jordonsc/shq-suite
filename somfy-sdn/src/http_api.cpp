#include "http_api.h"

#include <Arduino.h>
#include <ArduinoJson.h>
#include <HTTPUpdate.h>
#include <WebServer.h>
#include <WiFi.h>
#include <esp_app_desc.h>
#include <esp_ota_ops.h>
#include <esp_system.h>

#include <cstring>

#include "bus.h"
#include "devices.h"
#include "diag.h"
#include "fault.h"
#include "mono.h"
#include "sdn.h"
#include "version.h"
#include "wifi_prov.h"
#include "ws_api.h"

namespace http_api {

namespace {

WebServer* g_server = nullptr;

// Application identity — embedded in the native esp_app_desc (app_desc.cpp), surfaced in
// /stats + /stats.json, and enforced by the OTA guard (handleUpdate) so a foreign image
// (e.g. the actron-mitm firmware — same board, same /update endpoint) can't be flashed here.
constexpr const char* APP_ID = "somfy-sdn";
constexpr const char* APP_MODEL = "TinyC6";

const char* modeStr() { return (bus::mode() == bus::Mode::ACTIVE) ? "active" : "listen"; }

// Hardware reset cause of THIS boot — distinguishes a power cycle (poweron) from our own
// watchdog self-heals (sw + a `note=`) and from crashes (panic/wdt/brownout), so a fleet health
// sweep can tell what actually killed a controller. Pairs with wifi_prov::bootNote().
const char* resetReasonStr() {
  switch (esp_reset_reason()) {
    case ESP_RST_POWERON: return "poweron";
    case ESP_RST_SW: return "sw";
    case ESP_RST_PANIC: return "panic";
    case ESP_RST_INT_WDT: return "int_wdt";
    case ESP_RST_TASK_WDT: return "task_wdt";
    case ESP_RST_WDT: return "wdt";
    case ESP_RST_BROWNOUT: return "brownout";
    case ESP_RST_DEEPSLEEP: return "deepsleep";
    case ESP_RST_EXT: return "ext";
    default: return "other";
  }
}

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
  // (fw 1.5.0 — silent-socket telemetry ported from actron-sniffer, ledger shq-suite-0022)
  devices::DeviceTable& t = bus::table();
  const bus::Stats& st = bus::stats();
  size_t online = 0;
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (d && d->online) online++;
  }
  char buf[1024];
  snprintf(buf, sizeof(buf),
           "# app=%s mode=%s devices=%u online=%u "
           "tx=%u rx=%u polls=%u "
           "err.cksum=%u err.framing=%u err.timeout=%u err.nack=%u err.total=%u "
           // Resource/WS-churn telemetry for the ~5-day silent-socket fuse: fragmentation shows
           // as maxblk << heap; a growing ws_conn-ws_disc gap = sockets dying without the WS
           // library seeing a DISCONNECTED. reset/note say why THIS boot happened.
           "heap=%u minheap=%u maxblk=%u uptime=%lus "
           "ws=%u ws_conn=%u ws_disc=%u ws_err=%u "
           // Heartbeat health + clock-fault counters (fw 1.10.0, ledger shq-suite-0041).
           // hb_age should stay under ~10 s; anything larger means the state push has stalled
           // and HA is about to start its 40 s unavailable/available flap. clk_word counts
           // rejected high-word clock faults, clk_rebase counts clamps we abandoned because the
           // clock had genuinely moved, clk_rebase_ms is the signed size of the newest such step.
           "hb_age=%u hb_tx=%u clk_back=%u clk_word=%u clk_rebase=%u clk_rebase_ms=%ld "
           "clk_jump=%u clk_jumpms=%u "
           "wifi_disc=%u wifi_reason=%u reset=%s note=%s "
           // Self-diagnostics summary (ledger shq-suite-0038). `sock` is spare lwIP sockets — 0
           // means the pool HTTP and WS share is exhausted. `loop_max`/`http_max` are the worst
           // main-loop and HTTP-pump stalls since boot; anything near the 5 s pong deadline
           // explains an eviction. `pongto`/`peerclose`/`txerr` split the disconnects by who
           // caused them. Full records at /diag. READ clk_back AS A RATE: a healthy controller
           // gathers a few hundred over four days, and thousands per second is a clock pinned
           // right now — that is what a nine-hour Bed 2 outage looked like (shq-suite-0041).
           "sock=%u loop_max=%u http_max=%u stalls=%u "
           "pongto=%u peerclose=%u txerr=%u reaps=%u skipped=%u deferred=%u "
           // Station-side AP association (fw 1.8.0). The UniFi controller client list has
           // been seen disagreeing with the station; the station wins (wiki shq-network.md).
           "bssid=%s roams=%u diag_seq=%u "
           // Network-stack watchdog (fw 1.11.0, shq-suite-0044): consecutive unanswered gateway
           // probes, probes sent, re-associations performed, and the newest reason. nw_fail
           // climbing with the link up is the associated-but-unreachable signature.
           "nw_fail=%u nw_probes=%u nw_recover=%u nw_reason=%s "
           // Device-level fault (fw 1.10.0). "ok" when clear; otherwise the worst active code
           // and its one-line detail. This is what HA's fault sensor mirrors.
           "fault=%s fault_detail=\"%s\" "
           "rssi=%d ip=%s fw=\"%s\"",
           APP_ID, modeStr(), (unsigned)t.count(), (unsigned)online,
           st.tx_frames, st.rx_frames, st.polls,
           st.err_checksum, st.err_framing, st.err_timeout, st.err_nack,
           (unsigned)bus::errors().total(),
           (unsigned)ESP.getFreeHeap(), (unsigned)ESP.getMinFreeHeap(),
           (unsigned)ESP.getMaxAllocHeap(), (unsigned long)(mono::now() / 1000),
           (unsigned)ws_api::connectedClients(), (unsigned)ws_api::connectEvents(),
           (unsigned)ws_api::disconnectEvents(), (unsigned)ws_api::errorEvents(),
           (unsigned)ws_api::heartbeatAgeMs(), (unsigned)ws_api::heartbeatBroadcasts(),
           (unsigned)mono::backwardReads(), (unsigned)mono::wordSteps(),
           (unsigned)mono::rebases(), (long)mono::lastRebaseMs(),
           (unsigned)mono::forwardJumps(), (unsigned)mono::lastJumpMs(),
           (unsigned)wifi_prov::staDisconnectCount(), (unsigned)wifi_prov::lastDisconnectReason(),
           resetReasonStr(), wifi_prov::bootNote(),
           (unsigned)diag::spareSockets(), (unsigned)diag::loopMaxMs(), (unsigned)diag::httpMaxMs(),
           (unsigned)diag::loopStalls(), (unsigned)diag::pongTimeouts(), (unsigned)diag::peerCloses(),
           (unsigned)diag::transportErrors(), (unsigned)diag::stallReaps(),
           (unsigned)ws_api::skippedWrites(), (unsigned)ws_api::deferredReaps(),
           diag::currentBssid(), (unsigned)diag::wifiRoams(),
           (unsigned)diag::lastSeq(),
           (unsigned)wifi_prov::netProbeFailures(), (unsigned)wifi_prov::netProbes(),
           (unsigned)wifi_prov::netRecoveries(), wifi_prov::netLastReason(),
           fault::registry().worstSlug(), fault::registry().worstDetail(),
           WiFi.isConnected() ? WiFi.RSSI() : 0,
           WiFi.localIP().toString().c_str(), SOMFY_FW_VERSION " (" __DATE__ " " __TIME__ ")");
  return String(buf);
}

// Why the WS service dropped, and what the box looked like when it did (ledger shq-suite-0038).
// The same records HA receives over the socket, readable without a WS client — and reachable when
// the WS layer is precisely the thing that has stopped working.
static void handleDiag() {
  // ~6 kB transient rather than a permanent static: this endpoint is read by a human occasionally,
  // and the fault under investigation is memory pressure.
  constexpr size_t CAP = 6144;
  char* buf = (char*)malloc(CAP);
  if (buf == nullptr) {
    g_server->send(503, "text/plain", "# diag: out of memory\n");
    return;
  }
  diag::renderText(buf, CAP);
  g_server->send(200, "text/plain", buf);
  free(buf);
}

static void handleDiagJson() {
  JsonDocument doc;
  JsonObject health = doc["health"].to<JsonObject>();
  diag::healthToJson(health);
  health["ws_conn"] = ws_api::connectEvents();
  health["ws_disc"] = ws_api::disconnectEvents();
  health["ws_err"] = ws_api::errorEvents();
  health["hb_age_ms"] = ws_api::heartbeatAgeMs();
  health["hb_tx"] = ws_api::heartbeatBroadcasts();
  health["fw"] = SOMFY_FW_VERSION " " __DATE__ " " __TIME__;

  JsonArray arr = doc["events"].to<JsonArray>();
  for (uint32_t seq = diag::firstSeq(); seq != 0 && seq <= diag::lastSeq(); seq++) {
    const diag::Record* r = diag::bySeq(seq);
    if (r != nullptr) diag::toJson(*r, arr.add<JsonObject>());
  }

  String out;
  serializeJson(doc, out);
  g_server->send(200, "application/json", out);
}

// Human-friendly dashboard served at `/`. Static page; it polls /stats.json + /devices (JSON)
// and renders client-side, so the firmware doesn't string-build HTML on every request.
static const char INDEX_HTML[] PROGMEM = R"HTML(<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Somfy SDN controller</title>
<style>
  :root{--bg:#0f1419;--card:#1a212b;--line:#2b3543;--fg:#e6edf3;--mut:#8b98a5;--ok:#3fb950;--bad:#f85149;--warn:#d29922;--accent:#58a6ff}
  *{box-sizing:border-box}
  body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.5 system-ui,-apple-system,Segoe UI,Roboto,sans-serif}
  header{display:flex;align-items:baseline;gap:12px;flex-wrap:wrap;padding:18px 20px;border-bottom:1px solid var(--line)}
  header h1{font-size:18px;margin:0;font-weight:600}
  header .host{color:var(--accent);font-family:ui-monospace,monospace}
  header .fw{color:var(--mut);font-size:12px;margin-left:auto}
  main{padding:20px;max-width:1000px;margin:0 auto}
  .grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(175px,1fr));gap:12px;margin-bottom:22px}
  .cell{background:var(--card);border:1px solid var(--line);border-radius:8px;padding:12px 14px}
  .cell .k{color:var(--mut);font-size:11px;text-transform:uppercase;letter-spacing:.05em}
  .cell .v{font-size:18px;font-weight:600;margin-top:2px;font-family:ui-monospace,monospace;word-break:break-all}
  .badge{display:inline-block;padding:2px 9px;border-radius:99px;font-size:12px;font-weight:600}
  .b-ok{background:rgba(63,185,80,.15);color:var(--ok)} .b-bad{background:rgba(248,81,73,.15);color:var(--bad)}
  .b-warn{background:rgba(210,153,34,.15);color:var(--warn)} .b-mut{background:rgba(139,152,165,.15);color:var(--mut)}
  h2{font-size:13px;text-transform:uppercase;letter-spacing:.05em;color:var(--mut);margin:0 0 10px}
  table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--line);border-radius:8px;overflow:hidden}
  th,td{padding:10px 12px;text-align:left;border-bottom:1px solid var(--line);font-family:ui-monospace,monospace;font-size:13px}
  th{color:var(--mut);font-weight:600;font-size:11px;text-transform:uppercase;letter-spacing:.05em;font-family:system-ui}
  tr:last-child td{border-bottom:none}
  .muted{color:var(--mut)} .empty{color:var(--mut);padding:18px;text-align:center}
  footer{padding:14px 20px;color:var(--mut);font-size:12px;border-top:1px solid var(--line);text-align:center}
  footer a{color:var(--accent);text-decoration:none}
  .dim{opacity:.45}
</style></head>
<body>
<header>
  <h1>Somfy&nbsp;SDN <span class="host" id="host">…</span></h1>
  <span id="mode"></span>
  <span class="fw" id="fw"></span>
</header>
<main>
  <div class="grid" id="stats"></div>
  <h2>Motors</h2>
  <div id="motors"><div class="empty">loading…</div></div>
</main>
<footer>auto-refreshing every 3&nbsp;s · <a href="/help">raw endpoints</a> · <a href="/devices">/devices</a> · <a href="/stats.json">/stats.json</a></footer>
<script>
const $=s=>document.querySelector(s);
const esc=s=>String(s==null?"":s).replace(/[&<>]/g,c=>({"&":"&amp;","<":"&lt;",">":"&gt;"}[c]));
const cell=(k,v)=>`<div class="cell"><div class="k">${k}</div><div class="v">${v}</div></div>`;
const move=m=>m===1?"▲ up":m===2?"▼ down":"idle";
function pos(d){ if(d.position==null) return '<span class="muted">unknown</span>';
  return `${d.position}% <span class="muted">(${d.position==0?"open":d.position==100?"closed":"part"})</span>`; }
async function tick(){
  let s={},devs=[];
  try{ s=await (await fetch("/stats.json",{cache:"no-store"})).json();
       devs=await (await fetch("/devices",{cache:"no-store"})).json(); }
  catch(e){ $("#mode").innerHTML='<span class="badge b-bad">offline</span>'; return; }
  $("#host").textContent=s.hostname||s.ip||"";
  $("#fw").textContent=`fw ${s.fw} · ${s.build}`;
  $("#mode").innerHTML=s.mode==="active"?'<span class="badge b-ok">ACTIVE</span>':'<span class="badge b-warn">LISTEN</span>';
  const up=(s.uptime_s||0),hh=Math.floor(up/3600),mm=Math.floor(up%3600/60);
  $("#stats").innerHTML=[
    cell("IP",esc(s.ip)),cell("MAC",esc(s.mac)),
    cell("Wi-Fi",`${esc(s.ssid||"—")} <span class="muted">${s.rssi} dBm</span>`),
    cell("Motors",`${s.online}/${s.devices} <span class="muted">online</span>`),
    cell("WS clients",s.ws_clients),cell("Uptime",`${hh}h ${mm}m`),
    cell("Free heap",`${Math.round((s.heap_free||0)/1024)} kB`),
    cell("Bus traffic",`tx ${s.tx} · rx ${s.rx} · polls ${s.polls}`),
    cell("Wire errors",(s.err&&s.err.total)?`<span class="badge b-bad">${s.err.total}</span> <span class="muted">to ${s.err.timeout} ck ${s.err.cksum} fr ${s.err.framing} nk ${s.err.nack}</span>`:'<span class="badge b-ok">0</span>'),
  ].join("");
  if(!devs.length){ $("#motors").innerHTML='<div class="empty">no motors registered — bus may be LISTEN, motor offline, or needs a discovery sweep</div>'; return; }
  const rows=devs.map(d=>{
    const lim=d.limits_known?`${d.up_limit}–${d.down_limit}`:'<span class="muted">unset</span>';
    return `<tr class="${d.online?'':'dim'}">
      <td>${esc(d.addr)}${d.label?` <span class="muted">${esc(d.label)}</span>`:''}</td>
      <td>${pos(d)}</td><td>${move(d.moving)}</td>
      <td>${esc(d.direction)}</td><td>${lim}</td>
      <td>${d.fault?'<span class="badge b-bad">fault</span>':'<span class="badge b-ok">ok</span>'}</td>
      <td>${d.online?'<span class="badge b-ok">online</span>':'<span class="badge b-mut">offline</span>'}</td>
    </tr>`; }).join("");
  $("#motors").innerHTML=`<table><thead><tr><th>Address</th><th>Position</th><th>Motion</th><th>Direction</th><th>Limits (pulses)</th><th>Fault</th><th>Link</th></tr></thead><tbody>${rows}</tbody></table>`;
}
tick(); setInterval(tick,3000);
</script>
</body></html>)HTML";

void handleRoot() { g_server->send_P(200, "text/html", INDEX_HTML); }

void handleHelp() {
  String b = "Somfy SDN controller\n";
  b += statusLine() + "\n\n";
  b += "GET  /                            human-friendly status dashboard (HTML)\n";
  b += "GET  /stats                       status line (text)\n";
  b += "GET  /stats.json                  controller status (JSON, drives the dashboard)\n";
  b += "GET  /devices                     JSON device table\n";
  b += "GET  /log?since=<seq>&n=<max>      sniffed frames (incremental)\n";
  b += "GET  /errors?n=<max>              error ring (newest first)\n";
  b += "POST /mode?set=listen|active      TX gate (default LISTEN)\n";
  b += "POST /send?addr=AA:BB:CC&msg=0x03&data=04 28 00 00   build+send, report reply\n";
  b += "POST /discover                   discovery sweep (ACTIVE)\n";
  b += "POST /forget?addr=AA:BB:CC        remove a motor from the table\n";
  b += "POST /move?addr=AA:BB:CC&cmd=open|close|stop|pos|jogup|jogdown&value=<ha%|duration>\n";
  b += "POST /wifi?ssid=&password=        set creds, reboot\n";
  b += "POST /reconnect                  re-scan + reassociate to the strongest AP\n";
  b += "POST /update?url=<bin>            HTTP-pull OTA\n";
  b += "POST /clear                      reset ring buffers + counters\n";
  b += "POST /reboot?reason=<text>        deliberate restart (reason lands in next boot note=)\n";
  b += "\n# mutating endpoints are POST; curl -X POST (query args still parse)\n";
  g_server->send(200, "text/plain", b);
}

void handleStats() { g_server->send(200, "text/plain", statusLine() + "\n"); }

void handleStatsJson() {
  devices::DeviceTable& t = bus::table();
  const bus::Stats& st = bus::stats();
  size_t online = 0;
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (d && d->online) online++;
  }
  JsonDocument doc;
  doc["app"] = APP_ID;
  doc["model"] = APP_MODEL;
  doc["fw"] = SOMFY_FW_VERSION;
  doc["build"] = __DATE__ " " __TIME__;
  doc["mode"] = modeStr();
  doc["hostname"] = wifi_prov::hostname();
  doc["ip"] = WiFi.localIP().toString();
  doc["mac"] = WiFi.macAddress();
  doc["ssid"] = WiFi.SSID();
  doc["rssi"] = WiFi.isConnected() ? WiFi.RSSI() : 0;
  doc["uptime_s"] = (uint32_t)(mono::now() / 1000);
  doc["heap_free"] = (uint32_t)ESP.getFreeHeap();
  doc["heap_min"] = (uint32_t)ESP.getMinFreeHeap();
  doc["heap_maxblk"] = (uint32_t)ESP.getMaxAllocHeap();
  doc["reset_reason"] = resetReasonStr();
  doc["boot_note"] = wifi_prov::bootNote();
  doc["devices"] = (uint32_t)t.count();
  doc["online"] = (uint32_t)online;
  doc["ws_clients"] = (uint32_t)ws_api::connectedClients();
  doc["ws_conn"] = ws_api::connectEvents();
  doc["ws_disc"] = ws_api::disconnectEvents();
  doc["ws_err"] = ws_api::errorEvents();
  doc["hb_age_ms"] = ws_api::heartbeatAgeMs();
  doc["hb_tx"] = ws_api::heartbeatBroadcasts();
  JsonObject clk = doc["clk"].to<JsonObject>();
  clk["back"] = mono::backwardReads();
  clk["word_steps"] = mono::wordSteps();
  clk["last_word_units"] = mono::lastWordUnits();
  clk["rebases"] = mono::rebases();
  clk["last_rebase_ms"] = mono::lastRebaseMs();
  clk["jumps"] = mono::forwardJumps();
  clk["last_jump_ms"] = mono::lastJumpMs();
  JsonObject flt = doc["fault"].to<JsonObject>();
  flt["code"] = fault::registry().worstSlug();
  flt["detail"] = fault::registry().worstDetail();
  flt["mask"] = fault::registry().mask();
  JsonObject nw = doc["netwatch"].to<JsonObject>();
  nw["probe_failures"] = wifi_prov::netProbeFailures();
  nw["probes"] = wifi_prov::netProbes();
  nw["recoveries"] = wifi_prov::netRecoveries();
  nw["last_reason"] = wifi_prov::netLastReason();
  doc["wifi_disc"] = wifi_prov::staDisconnectCount();
  doc["wifi_reason"] = wifi_prov::lastDisconnectReason();
  doc["tx"] = st.tx_frames;
  doc["rx"] = st.rx_frames;
  doc["polls"] = st.polls;
  JsonObject err = doc["err"].to<JsonObject>();
  err["cksum"] = st.err_checksum;
  err["framing"] = st.err_framing;
  err["timeout"] = st.err_timeout;
  err["nack"] = st.err_nack;
  err["total"] = (uint32_t)bus::errors().total();
  String out;
  serializeJson(doc, out);
  g_server->send(200, "application/json", out);
}

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

void handleReconnect() {
  // Ack first; wifi_prov drops + re-scans from its loop() after this response has flushed.
  wifi_prov::requestReconnectBestAp();
  g_server->send(200, "text/plain", "# WiFi reconnect to strongest AP queued\n");
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

// Deliberate, operator-driven restart (fw 1.10.0). The WS command is the one HA drives; this
// exists because the WS server is exactly what dies in the failure modes worth rebooting for —
// during Bed 2's nine-hour clock wedge (shq-suite-0041) WS was dead the whole time while HTTP
// answered every request instantly. Out-of-band by design.
void handleReboot() {
  const String reason = g_server->hasArg("reason") ? g_server->arg("reason") : String("http");
  g_server->send(200, "text/plain", "# rebooting: " + reason + "\n");
  g_server->client().flush();
  delay(100);  // let the response leave before the stack goes down
  wifi_prov::noteReboot(reason.c_str());
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
  // Don't let httpUpdate reboot us automatically — we verify the written image's app identity
  // first (OTA app-guard). httpUpdate.update() still switches the boot partition on success;
  // we either boot it (id matches) or revert the boot partition (foreign image, never booted).
  httpUpdate.rebootOnUpdate(false);
  t_httpUpdate_return r = httpUpdate.update(client, url);

  if (r == HTTP_UPDATE_OK) {
    const esp_partition_t* next = esp_ota_get_boot_partition();
    esp_app_desc_t d{};
    if (next != nullptr && esp_ota_get_partition_description(next, &d) == ESP_OK &&
        strcmp(d.project_name, APP_ID) == 0) {
      Serial.printf("# OTA ok (app=%s ver=%s) — rebooting\n", d.project_name, d.version);
      delay(150);
      ESP.restart();
    } else {
      // Wrong-firmware guard: a non-somfy-sdn image (e.g. actron-mitm) reached this device.
      // Point the boot partition back at the running app so the foreign image is never executed.
      esp_ota_set_boot_partition(esp_ota_get_running_partition());
      Serial.printf("# OTA REJECTED: image app=\"%s\" != \"%s\" — reverted, staying on %s\n",
                    d.project_name, APP_ID, SOMFY_FW_VERSION);
      bus::otaResume();
    }
  } else {
    Serial.printf("# OTA failed (%d): %s\n", (int)r, httpUpdate.getLastErrorString().c_str());
    bus::otaResume();
  }
}

}  // namespace

void begin(uint16_t port) {
  g_server = new WebServer(port);
  g_server->on("/", HTTP_GET, handleRoot);
  g_server->on("/help", HTTP_GET, handleHelp);
  g_server->on("/stats", HTTP_GET, handleStats);
  g_server->on("/stats.json", HTTP_GET, handleStatsJson);
  g_server->on("/devices", HTTP_GET, handleDevices);
  g_server->on("/diag", HTTP_GET, handleDiag);
  g_server->on("/diag.json", HTTP_GET, handleDiagJson);
  g_server->on("/log", HTTP_GET, handleLog);
  g_server->on("/errors", HTTP_GET, handleErrors);
  g_server->on("/mode", HTTP_POST, handleMode);
  g_server->on("/send", HTTP_POST, handleSend);
  g_server->on("/discover", HTTP_POST, handleDiscover);
  g_server->on("/move", HTTP_POST, handleMove);
  g_server->on("/forget", HTTP_POST, handleForget);
  g_server->on("/wifi", HTTP_POST, handleWifi);
  g_server->on("/reconnect", HTTP_POST, handleReconnect);
  g_server->on("/update", HTTP_POST, handleUpdate);
  g_server->on("/clear", HTTP_POST, handleClear);
  g_server->on("/reboot", HTTP_POST, handleReboot);
  g_server->begin();
}

void loop() {
  if (g_server) g_server->handleClient();
}

}  // namespace http_api
