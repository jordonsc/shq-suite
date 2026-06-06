#include "wifi_prov.h"

#include <Arduino.h>
#include <DNSServer.h>
#include <ESPmDNS.h>
#include <Preferences.h>
#include <WebServer.h>
#include <WiFi.h>
#include <esp_mac.h>

#include <cstring>

#include "bus.h"
#include "sdn.h"
#include "version.h"

namespace wifi_prov {

namespace {

constexpr uint8_t PIN_BUTTON = 0;             // GPIO0 momentary button -> GND (SPEC §4)
constexpr uint32_t STA_CONNECT_TIMEOUT_MS = 15000;
constexpr uint8_t STA_RETRIES = 2;
constexpr uint32_t LONG_PRESS_MS = 10000;     // wipe creds
constexpr uint32_t SHORT_PRESS_MAX_MS = 1500; // wink

constexpr const char* NVS_NS = "somfy";
constexpr const char* NVS_SSID = "ssid";
constexpr const char* NVS_PASS = "pass";
constexpr const char* NVS_MOTORS = "motors";

Status g_status = Status::PORTAL;
char g_hostname[24] = "somfy-sdn";
WinkCb g_wink_cb = nullptr;

// Portal-mode servers (only created when no STA connection).
WebServer* g_portal = nullptr;
DNSServer* g_dns = nullptr;

// Button state.
bool g_btn_down = false;
uint32_t g_btn_down_ms = 0;

void computeHostname() {
  // Suffix from the last 2 octets of the WiFi STA MAC — the unique NIC-specific bytes, and the
  // same MAC `WiFi.macAddress()`/HA report, so the hostname matches the device's label/MAC.
  // (The earlier bug masked the low 16 bits of ESP.getEfuseMac(), which are the shared vendor
  // OUI -> every TinyC6 resolved to "4C40". getEfuseMac() also returns the *base* MAC, which on
  // the C6 differs from the STA MAC, so we read ESP_MAC_WIFI_STA explicitly for consistency.)
  uint8_t mac[6] = {0};
  esp_read_mac(mac, ESP_MAC_WIFI_STA);
  snprintf(g_hostname, sizeof(g_hostname), "somfy-sdn-%02X%02X", mac[4], mac[5]);
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
    MDNS.addService("somfy-sdn", "tcp", 8767);  // WS controller API (HA zeroconf service type)
    // TXT records consumed by the HA zeroconf config flow. `id` = the STA MAC (stable across
    // reboots and DHCP changes) — HA keys the config entry on it and rewrites the stored host
    // to the current IP whenever the device re-announces. See custom_components/somfy_sdn.
    String mac = WiFi.macAddress();
    mac.replace(":", "");
    mac.toLowerCase();
    MDNS.addServiceTxt("somfy-sdn", "tcp", "id", mac.c_str());
    MDNS.addServiceTxt("somfy-sdn", "tcp", "fw", SOMFY_FW_VERSION);
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
    "h2{margin:.2rem 0 1rem}h3{margin:1.6rem 0 .4rem}"
    "label{display:block;margin:.9rem 0 .25rem;font-weight:600}"
    "input,select{width:100%;padding:.55rem;font-size:1rem;border:1px solid #bbb;border-radius:.4rem}"
    "button{margin-top:1.3rem;padding:.65rem 1.1rem;font-size:1rem;border:0;border-radius:.4rem;"
    "background:#1565c0;color:#fff}"
    "a{color:#1565c0;text-decoration:none}.muted{color:#666;font-size:.85rem}"
    "ul{padding-left:1.1rem;margin:.3rem 0}.tools{margin-top:.5rem;font-size:.9rem}"
    ".pwrow{display:flex;gap:.4rem;align-items:stretch}.pwrow input{flex:1}"
    ".pwrow button{margin:0;background:#eee;color:#222;border:1px solid #bbb;font-size:1.1rem;padding:.4rem .7rem}"
    "</style>";

String buildSsidOptions() {
  // Synchronous scan — reliable and snappy (~1.5-2 s), no async state machine to wedge in
  // AP_STA. Dedupe by SSID keeping the strongest RSSI, then sort by signal descending.
  constexpr int MAXAP = 40;
  String names[MAXAP];
  int rssis[MAXAP];
  int cnt = 0;

  int n = WiFi.scanNetworks(false /*async*/, false /*show_hidden*/);
  for (int i = 0; i < n; i++) {
    String s = WiFi.SSID(i);
    if (s.length() == 0) continue;  // hidden/blank — use the manual field
    int r = WiFi.RSSI(i);
    int found = -1;
    for (int j = 0; j < cnt; j++) {
      if (names[j] == s) { found = j; break; }
    }
    if (found >= 0) {
      if (r > rssis[found]) rssis[found] = r;  // keep strongest BSSID for this SSID
    } else if (cnt < MAXAP) {
      names[cnt] = s;
      rssis[cnt] = r;
      cnt++;
    }
  }
  WiFi.scanDelete();  // free results so the next scan can start cleanly

  if (cnt == 0) return "<option disabled selected>no networks found</option>";

  // Selection sort by RSSI descending (cnt is small).
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

String buildMotorList() {
  devices::DeviceTable& t = bus::table();
  if (t.count() == 0) {
    return "<p class=muted>None detected yet. The controller auto-detects motors once it's on "
           "your network; you can also <a href=/scanmotors>scan the bus now</a>.</p>";
  }
  String out = "<ul>";
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (!d) continue;
    char a[9];
    sdn::formatAddress(d->addr, a);
    out += "<li><code>" + String(a) + "</code> — " + (d->online ? "online" : "offline") + "</li>";
  }
  out += "</ul>";
  return out;
}

void portalRoot() {
  String h = "<!doctype html><html><head><meta charset=utf-8>"
             "<meta name=viewport content='width=device-width,initial-scale=1'>"
             "<title>somfy-sdn setup</title>";
  h += PORTAL_CSS;
  h += "</head><body><h2>Somfy SDN controller</h2>";
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
  h += "<h3>Detected motors</h3>";
  h += buildMotorList();
  h += "<div class=tools><a href=/scanmotors>↻ Scan the bus for motors</a></div>";
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

// Explicit, user-initiated bus scan during onboarding: briefly arm ACTIVE, run a discovery
// sweep, then drop back to LISTEN (preserving the boot TX-safety default). The motor table is
// informational here — the controller re-detects automatically once it's on the network.
void portalScanMotors() {
  bus::Mode prev = bus::mode();
  bus::setMode(bus::Mode::ACTIVE);
  bus::Command c;
  c.type = bus::CmdType::REDISCOVER;
  c.broadcast = true;
  c.job_id = bus::nextJobId();
  bus::enqueue(c);
  delay(1600);  // let the sweep complete
  bus::setMode(prev);  // restore (don't clobber the boot/operator mode)
  g_portal->sendHeader("Location", "/");
  g_portal->send(303);
}

void startPortal() {
  // AP_STA so we can scan for nearby networks (synchronously, per page load) while serving AP.
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
  g_portal->on("/scanmotors", portalScanMotors);
  g_portal->onNotFound(portalRoot);  // captive-portal catch-all
  g_portal->begin();
}

// ---- GPIO0 button --------------------------------------------------------

void serviceButton() {
  bool pressed = (digitalRead(PIN_BUTTON) == LOW);
  uint32_t now = millis();
  if (pressed && !g_btn_down) {
    g_btn_down = true;
    g_btn_down_ms = now;
  } else if (pressed && g_btn_down) {
    if (now - g_btn_down_ms >= LONG_PRESS_MS) {
      Serial.println("# GPIO0 long-press: wiping WiFi credentials");
      factoryWipe();  // reboots
    }
  } else if (!pressed && g_btn_down) {
    uint32_t held = now - g_btn_down_ms;
    g_btn_down = false;
    if (held <= SHORT_PRESS_MAX_MS) {
      Serial.println("# GPIO0 short-press: wink all motors");
      if (g_wink_cb) g_wink_cb();
    }
  }
}

}  // namespace

Status begin() {
  computeHostname();
  pinMode(PIN_BUTTON, INPUT_PULLUP);

  String ssid, pass;
  if (readCreds(ssid, pass) && tryConnect(ssid, pass)) {
    g_status = Status::CONNECTED;
    Serial.printf("# WiFi up: http://%s/  (%s)\n", WiFi.localIP().toString().c_str(),
                  g_hostname);
    startMdns();
  } else {
    g_status = Status::PORTAL;
    startPortal();
  }
  return g_status;
}

void loop() {
  serviceButton();
  if (g_status == Status::PORTAL) {
    if (g_dns) g_dns->processNextRequest();
    if (g_portal) g_portal->handleClient();
  }
}

Status status() { return g_status; }
bool isConnected() { return g_status == Status::CONNECTED && WiFi.isConnected(); }
const char* hostname() { return g_hostname; }

bool saveCredentials(const char* ssid, const char* password) {
  Preferences p;
  if (!p.begin(NVS_NS, false)) return false;
  p.putString(NVS_SSID, ssid);
  p.putString(NVS_PASS, password ? password : "");
  p.end();
  delay(300);
  ESP.restart();
  return true;  // unreachable
}

void factoryWipe() {
  Preferences p;
  p.begin(NVS_NS, false);
  p.clear();
  p.end();
  delay(300);
  ESP.restart();
}

void loadConfiguredMotors() {
  Preferences p;
  if (!p.begin(NVS_NS, true)) return;
  String motors = p.getString(NVS_MOTORS, "");
  p.end();
  if (motors.length() == 0) return;

  int start = 0;
  while (start < (int)motors.length()) {
    int comma = motors.indexOf(',', start);
    String tok = (comma < 0) ? motors.substring(start) : motors.substring(start, comma);
    tok.trim();
    uint8_t addr[3];
    if (sdn::parseAddress(tok.c_str(), addr)) {
      bus::addConfiguredDevice(addr);
      Serial.printf("# configured motor %s\n", tok.c_str());
    }
    if (comma < 0) break;
    start = comma + 1;
  }
}

void setWinkCallback(WinkCb cb) { g_wink_cb = cb; }

}  // namespace wifi_prov
