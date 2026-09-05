#include "wifi_prov.h"

#include <Arduino.h>
#include <DNSServer.h>
#include <ESPmDNS.h>
#include <Preferences.h>
#include <WebServer.h>
#include <WiFi.h>
#include <esp_mac.h>
#include <esp_phy_init.h>
#include <esp_wifi.h>

#include "lwip/ip_addr.h"
#include "ping/ping_sock.h"

#include "diag.h"
#include "mono.h"
#include "netwatch.h"
#include "txstats.h"

namespace wifi_prov {

namespace {

// Network-stack watchdog glue (fw 1.11.0, ledger shq-suite-0044; the policy itself is the pure
// netwatch::Policy — twin of somfy-sdn/src/wifi_prov.cpp, keep in step). One ICMP echo to the
// DHCP gateway every NET_PROBE_INTERVAL_MS via the IDF's esp_ping (a short-lived task per probe;
// the session frees itself in on_ping_end). The probe is deliberately the GATEWAY and not HA: an
// HA outage must never look like a dead stack. A probe whose callbacks never arrive is counted
// as failed after NET_PROBE_GIVEUP_MS. THE REBOOT TIER IS OFF ON THIS DEVICE (Policy(false)):
// the bridge sits on a physically cut RS485 bus and must never reboot with the A/C running
// (shq-suite-0042), so a persistent fault repeats the re-association instead — which only
// touches WiFi and leaves the relay untouched.
constexpr uint32_t NET_PROBE_INTERVAL_MS = 60 * 1000;
constexpr uint32_t NET_PROBE_TIMEOUT_MS = 2000;
constexpr uint32_t NET_PROBE_GIVEUP_MS = 10 * 1000;
constexpr uint32_t NET_TICK_MS = 1000;

// NOTE: unlike the somfy port, there is NO GPIO0 button here — GPIO0 is the NEO-side UART0 RX
// (PIN_B_RX in main.cpp). Reconfiguring it as a button input kills NEO reception (B.frames=0,
// no A/C state to decode). Creds are wiped via the HTTP POST /wifireset endpoint instead.
constexpr uint32_t STA_CONNECT_TIMEOUT_MS = 15000;
constexpr uint8_t STA_RETRIES = 2;

constexpr const char* NVS_NS = "actron";
constexpr const char* NVS_SSID = "ssid";
constexpr const char* NVS_PASS = "pass";

Status g_status = Status::PORTAL;
char g_hostname[24] = "actron-mitm";

// Portal-mode servers (only created when no STA connection).
WebServer* g_portal = nullptr;
DNSServer* g_dns = nullptr;

// Set by requestReconnectBestAp() and serviced from loop() so a calling HTTP ack flushes first.
bool g_reconnect_requested = false;

// WiFi protocol A/B knob (fw 1.13.0, wifi_proto.h). Cached from NVS in begin(); applied by
// applyProto() right before every WiFi.begin(). The Arduino core does not re-apply a protocol
// bitmap of its own here: WiFiGenericClass::mode() only rewrites it when toggling long-range
// mode (WiFiGeneric.cpp `_wifi_disable_lr`), and the driver's post-init default is B|G|N|AX
// (esp_wifi.h). esp_wifi_set_protocol() needs esp_wifi_init() to have run, which WiFi.mode()
// guarantees, and the value survives disconnect/connect — it would only be lost on
// WiFi.mode(WIFI_OFF), which this firmware never calls.
wifi_proto::Proto g_proto = wifi_proto::Proto::BGNAX;
static_assert(wifi_proto::bitmap(wifi_proto::Proto::BGN) ==
                  (WIFI_PROTOCOL_11B | WIFI_PROTOCOL_11G | WIFI_PROTOCOL_11N),
              "wifi_proto bitmap drifted from the IDF macros");
static_assert(wifi_proto::bitmap(wifi_proto::Proto::BG) == (WIFI_PROTOCOL_11B | WIFI_PROTOCOL_11G),
              "wifi_proto bitmap drifted from the IDF macros");
static_assert(wifi_proto::bitmap(wifi_proto::Proto::BGNAX) ==
                  (WIFI_PROTOCOL_11B | WIFI_PROTOCOL_11G | WIFI_PROTOCOL_11N | WIFI_PROTOCOL_11AX),
              "bgnax bitmap must match the IDF macros");

void readProto() {
  Preferences p;
  if (!p.begin(NVS_NS, true)) return;
  String v = p.getString(wifi_proto::NVS_KEY, "");
  p.end();
  wifi_proto::Proto parsed = wifi_proto::Proto::BGNAX;
  if (v.length() > 0 && !wifi_proto::parse(v.c_str(), &parsed)) {
    Serial.printf("# WiFi: ignoring unknown wifi_proto \"%s\" in NVS — using bgnax\n", v.c_str());
  }
  g_proto = parsed;
}

// Called after WiFi.mode(WIFI_STA) and before WiFi.begin(). Every setting, the default included,
// is written explicitly (see below); the value the driver had persisted is never trusted.
void applyProto() {
  // Always write the bitmap, even for the default: the IDF driver persists the last value in its
  // own NVS namespace (CONFIG_ESP_WIFI_NVS_ENABLED), so a unit that was ever switched to bgn would
  // otherwise stay HT20 for ever after "bgnax" (found on Living Back, ledger shq-suite-0046).
  const uint8_t bm = wifi_proto::bitmap(g_proto);
  const esp_err_t err = esp_wifi_set_protocol(WIFI_IF_STA, bm);
  if (err != ESP_OK) {
    Serial.printf("# WiFi: esp_wifi_set_protocol(%s=0x%02x) failed: %s\n", wifi_proto::name(g_proto),
                  (unsigned)bm, esp_err_to_name(err));
  } else {
    Serial.printf("# WiFi: protocol %s (0x%02x)\n", wifi_proto::name(g_proto), (unsigned)bm);
  }
}

// Network-stack watchdog state (fw 1.11.0). The ping callbacks run on the ping task; they only
// write g_probe_result, which the 1 Hz tick in loop() consumes.
netwatch::Policy g_netwatch(false);  // no reboot tier on the actron bridge — see above
volatile uint8_t g_probe_result = 0;       // 0 = nothing new, 1 = answered, 2 = unanswered
bool g_probe_inflight = false;
uint32_t g_probe_started_ms = 0;
uint32_t g_last_probe_ms = 0;
uint32_t g_probes_sent = 0;
uint32_t g_last_net_tick_ms = 0;
uint32_t g_inbound_ms = 0;                 // mono::now() of the last inbound WS frame/pong
bool g_inbound_seen = false;

void onPingSuccess(esp_ping_handle_t, void*) { g_probe_result = 1; }
void onPingTimeout(esp_ping_handle_t, void*) {
  if (g_probe_result == 0) g_probe_result = 2;
}
void onPingEnd(esp_ping_handle_t h, void*) {
  if (g_probe_result == 0) g_probe_result = 2;
  esp_ping_delete_session(h);
}

// Fire one echo request at the gateway. Failure to even create the session is reported as an
// unanswered probe: it means the stack could not find the memory for a socket and a task, which
// is precisely the condition the watchdog exists for.
void startGatewayProbe() {
  g_probes_sent++;
  g_probe_result = 0;
  g_probe_inflight = true;
  g_probe_started_ms = mono::now();
  const IPAddress gw = WiFi.gatewayIP();
  if ((uint32_t)gw == 0) {
    g_probe_result = 2;
    return;
  }
  esp_ping_config_t cfg = ESP_PING_DEFAULT_CONFIG();
  ip_addr_set_ip4_u32(&cfg.target_addr, (uint32_t)gw);
  cfg.count = 1;
  cfg.interval_ms = 1000;
  cfg.timeout_ms = NET_PROBE_TIMEOUT_MS;
  cfg.data_size = 32;
  esp_ping_callbacks_t cbs = {};
  cbs.cb_args = nullptr;
  cbs.on_ping_success = onPingSuccess;
  cbs.on_ping_timeout = onPingTimeout;
  cbs.on_ping_end = onPingEnd;
  esp_ping_handle_t h = nullptr;
  if (esp_ping_new_session(&cfg, &cbs, &h) != ESP_OK || h == nullptr) {
    g_probe_result = 2;
    return;
  }
  if (esp_ping_start(h) != ESP_OK) {
    esp_ping_delete_session(h);
    g_probe_result = 2;
  }
}

void computeHostname() {
  // Suffix from the last 2 octets of the WiFi STA MAC (the unique, NIC-specific bytes — the same
  // MAC WiFi.macAddress()/HA report). Read ESP_MAC_WIFI_STA explicitly; ESP.getEfuseMac() returns
  // the base MAC (differs from the STA MAC on the C6) and its low bytes are the shared vendor OUI.
  uint8_t mac[6] = {0};
  esp_read_mac(mac, ESP_MAC_WIFI_STA);
  snprintf(g_hostname, sizeof(g_hostname), "actron-mitm-%02X%02X", mac[4], mac[5]);
}

bool readCreds(String& ssid, String& pass) {
  Preferences p;
  if (!p.begin(NVS_NS, true)) return false;
  ssid = p.getString(NVS_SSID, "");
  pass = p.getString(NVS_PASS, "");
  p.end();
  return ssid.length() > 0;
}

bool tryConnect(const String& ssid, const String& pass) {
  WiFi.mode(WIFI_STA);
  WiFi.setHostname(g_hostname);
  WiFi.setSleep(false);
  // Join the STRONGEST AP for the SSID, not the first found (Arduino default WIFI_FAST_SCAN
  // sticks to a cached/distant BSSID across reboots). All-channel scan + sort-by-signal.
  WiFi.setScanMethod(WIFI_ALL_CHANNEL_SCAN);
  WiFi.setSortMethod(WIFI_CONNECT_AP_BY_SIGNAL);
  applyProto();  // fw 1.13.0: after mode() (esp_wifi_init done), before begin()
  for (uint8_t attempt = 0; attempt <= STA_RETRIES; attempt++) {
    Serial.printf("# WiFi connecting to \"%s\" (attempt %d)", ssid.c_str(), attempt + 1);
    WiFi.begin(ssid.c_str(), pass.c_str());
    uint32_t t0 = millis();
    while (!WiFi.isConnected() && millis() - t0 < STA_CONNECT_TIMEOUT_MS) {
      delay(250);
      Serial.print('.');
    }
    Serial.println();
    if (WiFi.isConnected()) return true;
    WiFi.disconnect();
  }
  return false;
}

void startMdns() {
  if (MDNS.begin(g_hostname)) {
    MDNS.addService("http", "tcp", 80);
    // Distinct service type for positive identification (somfy advertises _somfy-sdn._tcp). The
    // actron_mitm_controller HA integration uses a manual IP, so this is identity-only.
    MDNS.addService("actron-mitm", "tcp", 8767);
    String mac = WiFi.macAddress();
    mac.replace(":", "");
    mac.toLowerCase();
    MDNS.addServiceTxt("actron-mitm", "tcp", "id", mac.c_str());
  }
}

// ---- captive portal ------------------------------------------------------

String htmlEscape(const String& s) {
  String o;
  o.reserve(s.length() + 8);
  for (size_t i = 0; i < s.length(); i++) {
    char c = s[i];
    if (c == '&') o += "&amp;";
    else if (c == '<') o += "&lt;";
    else if (c == '>') o += "&gt;";
    else if (c == '"') o += "&quot;";
    else o += c;
  }
  return o;
}

const char* PORTAL_CSS =
    "<style>"
    "*{box-sizing:border-box}"
    "body{font-family:system-ui,-apple-system,sans-serif;margin:0;padding:1.5rem;"
    "max-width:34rem;color:#1a1a1a}"
    "h2{margin:.2rem 0 1rem}"
    "label{display:block;margin:.9rem 0 .25rem;font-weight:600}"
    "input,select{width:100%;padding:.55rem;font-size:1rem;border:1px solid #bbb;border-radius:.4rem}"
    "button{margin-top:1.3rem;padding:.65rem 1.1rem;font-size:1rem;border:0;border-radius:.4rem;"
    "background:#1565c0;color:#fff}"
    "a{color:#1565c0;text-decoration:none}.muted{color:#666;font-size:.85rem}"
    ".tools{margin-top:.5rem;font-size:.9rem}"
    ".pwrow{display:flex;gap:.4rem;align-items:stretch}.pwrow input{flex:1}"
    ".pwrow button{margin:0;background:#eee;color:#222;border:1px solid #bbb;font-size:1.1rem;padding:.4rem .7rem}"
    "</style>";

String buildSsidOptions() {
  // Synchronous scan — reliable and snappy (~1.5-2 s). Dedupe by SSID keeping strongest RSSI.
  constexpr int MAXAP = 40;
  String names[MAXAP];
  int rssis[MAXAP];
  int cnt = 0;

  int n = WiFi.scanNetworks(false /*async*/, false /*show_hidden*/);
  for (int i = 0; i < n; i++) {
    String s = WiFi.SSID(i);
    if (s.length() == 0) continue;
    int r = WiFi.RSSI(i);
    int found = -1;
    for (int j = 0; j < cnt; j++) {
      if (names[j] == s) { found = j; break; }
    }
    if (found >= 0) {
      if (r > rssis[found]) rssis[found] = r;
    } else if (cnt < MAXAP) {
      names[cnt] = s;
      rssis[cnt] = r;
      cnt++;
    }
  }
  WiFi.scanDelete();

  if (cnt == 0) return "<option disabled selected>no networks found</option>";

  for (int a = 0; a < cnt; a++) {
    for (int b = a + 1; b < cnt; b++) {
      if (rssis[b] > rssis[a]) {
        int tr = rssis[a]; rssis[a] = rssis[b]; rssis[b] = tr;
        String ts = names[a]; names[a] = names[b]; names[b] = ts;
      }
    }
  }

  String out;
  for (int i = 0; i < cnt; i++) {
    String esc = htmlEscape(names[i]);
    out += "<option value=\"" + esc + "\">" + esc + "  (" + String(rssis[i]) + " dBm)</option>";
  }
  return out;
}

void portalRoot() {
  String h = "<!doctype html><html><head><meta charset=utf-8>"
             "<meta name=viewport content='width=device-width,initial-scale=1'>"
             "<title>actron-mitm setup</title>";
  h += PORTAL_CSS;
  h += "</head><body><h2>Actron A/C bridge</h2>";
  h += "<p class=muted>WiFi setup for the Actron MITM controller.</p>";
  h += "<form method=POST action=/save>";
  h += "<label for=ssid>WiFi network</label>";
  h += "<select name=ssid id=ssid onchange=\"document.getElementById('mrow').style.display="
       "(this.value=='__other__')?'block':'none'\">";
  h += buildSsidOptions();
  h += "<option value=__other__>Other / hidden network…</option></select>";
  h += "<div id=mrow style='display:none'><label for=ssid_manual>Hidden SSID</label>"
       "<input name=ssid_manual id=ssid_manual autocomplete=off></div>";
  h += "<label for=pass>Password</label>";
  h += "<div class=pwrow><input name=pass id=pass type=password autocomplete=off>";
  h += "<button type=button aria-label='Show password' onclick=\"var p=document.getElementById('pass');"
       "var s=p.type==='password';p.type=s?'text':'password';this.textContent=s?'🙈':'👁';\">👁</button></div>";
  h += "<button type=submit>Save &amp; reboot</button>";
  h += "</form>";
  h += "<div class=tools><a href=/>↻ Rescan WiFi networks</a></div>";
  h += "</body></html>";
  g_portal->send(200, "text/html", h);
}

void portalSave() {
  String ssid = g_portal->arg("ssid");
  if (ssid == "__other__") ssid = g_portal->arg("ssid_manual");
  ssid.trim();
  String pass = g_portal->arg("pass");
  if (ssid.length() == 0) {
    g_portal->send(400, "text/plain", "SSID required\n");
    return;
  }
  Preferences p;
  p.begin(NVS_NS, false);
  p.putString(NVS_SSID, ssid);
  p.putString(NVS_PASS, pass);
  p.end();
  g_portal->send(200, "text/html",
                 String("<!doctype html>") + PORTAL_CSS +
                     "<body><h2>Saved</h2><p>Joining <b>" + htmlEscape(ssid) +
                     "</b> — rebooting…</p></body>");
  delay(800);
  ESP.restart();
}

void startPortal() {
  // AP_STA so we can scan for nearby networks (synchronously, per page load) while serving the AP.
  WiFi.mode(WIFI_AP_STA);
  WiFi.softAP(g_hostname);
  IPAddress ip = WiFi.softAPIP();
  Serial.printf("# Provisioning portal: connect to AP \"%s\", browse to http://%s/\n",
                g_hostname, ip.toString().c_str());

  g_dns = new DNSServer();
  g_dns->start(53, "*", ip);  // captive: resolve everything to us

  g_portal = new WebServer(80);
  g_portal->on("/", portalRoot);
  g_portal->on("/save", HTTP_POST, portalSave);
  g_portal->onNotFound(portalRoot);  // captive-portal catch-all
  g_portal->begin();
}

// Drop the association and rejoin. Shared by the manual reconnect and the network-stack
// watchdog: this is the exact operation that brought the Bed 2 somfy controller back from the
// dead on 2026-09-04 (a router-side kick) — the WiFi driver tears down and re-arms its timers
// against the current clock and frees what it had queued. tryConnect blocks the main loop for a
// few seconds; the bridge task keeps relaying. WiFi only — the RS485 relay is untouched.
void reassociate(const char* why) {
  if (g_status != Status::CONNECTED) return;
  String ssid, pass;
  if (!readCreds(ssid, pass)) return;
  Serial.printf("# WiFi: reassociating (%s; was rssi=%d heap=%u) — re-scanning\n", why,
                (int)WiFi.RSSI(), (unsigned)ESP.getFreeHeap());
  delay(250);
  WiFi.disconnect();
  delay(100);
  tryConnect(ssid, pass);
  Serial.printf("# WiFi: reconnected ip=%s rssi=%d heap=%u\n",
                WiFi.localIP().toString().c_str(), (int)WiFi.RSSI(),
                (unsigned)ESP.getFreeHeap());
}

void serviceReconnect() {
  if (!g_reconnect_requested) return;
  g_reconnect_requested = false;
  reassociate("manual");
}

// The 1 Hz network-stack watchdog tick: launch/collect the gateway probe, feed the policy, act.
void serviceNetwatch() {
  if (g_status != Status::CONNECTED) return;
  const uint32_t now = mono::now();
  if ((uint32_t)(now - g_last_net_tick_ms) < NET_TICK_MS) return;
  g_last_net_tick_ms = now;

  netwatch::Probe probe = netwatch::Probe::None;
  if (g_probe_inflight) {
    const uint8_t r = g_probe_result;
    if (r != 0) {
      probe = (r == 1) ? netwatch::Probe::Ok : netwatch::Probe::Fail;
      g_probe_inflight = false;
    } else if ((uint32_t)(now - g_probe_started_ms) >= NET_PROBE_GIVEUP_MS) {
      probe = netwatch::Probe::Fail;  // callbacks never came: the ping task could not run
      g_probe_inflight = false;
    }
  }
  const bool link_up = WiFi.isConnected();
  if (link_up && !g_probe_inflight &&
      (g_last_probe_ms == 0 || (uint32_t)(now - g_last_probe_ms) >= NET_PROBE_INTERVAL_MS)) {
    g_last_probe_ms = now;
    startGatewayProbe();
  }

  // MAC-layer transmit counters (fw 1.12.0, txstats.h): enabled on first link-up, read and
  // cleared once a second. Read-only telemetry, sharing this tick so nothing new is polled.
  txstats::tick(now, link_up);

  netwatch::Input in;
  in.now_ms = now;
  in.link_up = link_up;
  in.probe = probe;
  in.rebases = mono::rebases();
  in.last_rebase_ms = mono::lastRebaseMs();
  in.heap = ESP.getFreeHeap();
  in.inbound_age_ms = g_inbound_seen ? (uint32_t)(now - g_inbound_ms) : 0xFFFFFFFFu;

  const netwatch::Verdict v = g_netwatch.step(in);
  if (v.action == netwatch::Action::Reassociate) {
    Serial.printf("# netwatch: %s — reassociating (fails=%u heap=%u)\n", v.reason,
                  (unsigned)g_netwatch.consecutiveFailures(), (unsigned)in.heap);
    diag::noteNetRecover(v.reason, g_netwatch.recoveries());
    reassociate(v.reason);
    g_probe_inflight = false;
    g_probe_result = 0;
    g_last_probe_ms = 0;
  }
  // Action::Reboot cannot be issued: the policy was built with allow_reboot=false.
}

}  // namespace

Status begin() {
  computeHostname();
  readProto();  // fw 1.13.0 WiFi protocol A/B knob; tryConnect applies it

  String ssid, pass;
  if (readCreds(ssid, pass) && tryConnect(ssid, pass)) {
    g_status = Status::CONNECTED;
    Serial.printf("# WiFi up: http://%s/  (%s)\n", WiFi.localIP().toString().c_str(), g_hostname);
    startMdns();
  } else {
    g_status = Status::PORTAL;
    startPortal();
  }
  return g_status;
}

void loop() {
  serviceReconnect();
  serviceNetwatch();
  if (g_status == Status::PORTAL) {
    if (g_dns) g_dns->processNextRequest();
    if (g_portal) g_portal->handleClient();
  }
}

void requestReconnectBestAp() { g_reconnect_requested = true; }

void noteInbound() {
  g_inbound_ms = mono::now();
  g_inbound_seen = true;
}
uint32_t netProbeFailures() { return g_netwatch.consecutiveFailures(); }
uint32_t netRecoveries() { return g_netwatch.recoveries(); }
uint32_t netProbes() { return g_probes_sent; }
const char* netLastReason() { return g_netwatch.lastReason(); }

wifi_proto::Proto wifiProto() { return g_proto; }

bool setWifiProto(wifi_proto::Proto proto) {
  Preferences p;
  if (!p.begin(NVS_NS, false)) return false;
  const size_t n = p.putString(wifi_proto::NVS_KEY, wifi_proto::name(proto));
  p.end();
  if (n == 0) return false;
  g_proto = proto;
  Serial.printf("# WiFi: wifi_proto=%s saved (rebooting to apply)\n",
                wifi_proto::name(proto));
  return true;
}

bool erasePhyCalibration() {
  const esp_err_t err = esp_phy_erase_cal_data_in_nvs();
  Serial.printf("# PHY: erase calibration data in NVS -> %s\n", esp_err_to_name(err));
  return err == ESP_OK;
}

Status status() { return g_status; }
bool isConnected() { return g_status == Status::CONNECTED && WiFi.isConnected(); }
const char* hostname() { return g_hostname; }

void factoryWipe() {
  Preferences p;
  p.begin(NVS_NS, false);
  p.clear();
  p.end();
  delay(300);
  ESP.restart();
}

}  // namespace wifi_prov
