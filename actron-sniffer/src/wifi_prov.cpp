#include "wifi_prov.h"

#include <Arduino.h>
#include <DNSServer.h>
#include <ESPmDNS.h>
#include <Preferences.h>
#include <WebServer.h>
#include <WiFi.h>
#include <esp_mac.h>

namespace wifi_prov {

namespace {

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

void serviceReconnect() {
  if (!g_reconnect_requested) return;
  g_reconnect_requested = false;
  if (g_status != Status::CONNECTED) return;
  String ssid, pass;
  if (!readCreds(ssid, pass)) return;
  Serial.printf("# WiFi: manual reconnect (was rssi=%d) — re-scanning\n", (int)WiFi.RSSI());
  delay(250);
  WiFi.disconnect();
  delay(100);
  tryConnect(ssid, pass);
  Serial.printf("# WiFi: reconnected ip=%s rssi=%d\n", WiFi.localIP().toString().c_str(),
                (int)WiFi.RSSI());
}

}  // namespace

Status begin() {
  computeHostname();

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
  if (g_status == Status::PORTAL) {
    if (g_dns) g_dns->processNextRequest();
    if (g_portal) g_portal->handleClient();
  }
}

void requestReconnectBestAp() { g_reconnect_requested = true; }

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
