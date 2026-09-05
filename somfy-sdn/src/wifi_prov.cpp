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

#include <cstring>

#include "lwip/ip_addr.h"
#include "ping/ping_sock.h"

#include "bus.h"
#include "diag.h"
#include "mono.h"
#include "netwatch.h"
#include "sdn.h"
#include "txstats.h"
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
constexpr const char* NVS_BOOTNOTE = "bootnote";

// WiFi-death watchdog (fw 1.5.0, ledger shq-suite-0022). Three somfy controllers wedged
// permanently OFF the network (ICMP-dead, needed a breaker power-cycle): after the one-shot boot
// connect, nothing in this firmware ever re-drives a dead STA link — it relies entirely on the
// Arduino stack's implicit auto-reconnect, and when that gives up (or the WiFi task itself
// wedges) the device is stranded with no remote recovery. A reboot re-runs the full provisioning
// connect (all-channel scan, retries), so: STA link continuously down this long => reboot.
// Generous enough to ride out an AP restart (~2-3 min) without churning.
constexpr uint32_t WIFI_DEAD_REBOOT_MS = 5 * 60 * 1000;

// Portal-purgatory retry (fw 1.5.0). If the AP is down when the device boots (e.g. after a power
// outage where the AP comes up slower than the ESP32), tryConnect fails and the device falls into
// the SoftAP portal — and previously stayed there FOREVER despite holding valid creds. With creds
// present, reboot periodically to retake the STA path; the window is long enough to finish a
// manual re-provision in the portal first. A creds-less portal (fresh device / post-wipe) never
// retries — nothing to retry with.
constexpr uint32_t PORTAL_RETRY_MS = 15 * 60 * 1000;

// Active link-retry loop (fw 1.5.0). Between "link dropped" and the reboot backstop, don't just
// trust the Arduino stack's implicit auto-reconnect — it re-drives the SAME association state
// machine, which is precisely what gets stuck after an infra outage (AP rebooted/rekeyed/changed
// channel mid-association: a known ESP32 glitch class where auto-reconnect spins forever and only
// a fresh WiFi.begin() — or a reboot — recovers). After LINK_RETRY_AFTER_MS of continuous
// downtime (long enough for auto-reconnect to win the easy cases itself), force a full
// disconnect + begin() every LINK_RETRY_INTERVAL_MS. begin() re-applies the all-channel
// strongest-AP scan, so it also copes with the AP coming back on a new channel/BSSID.
constexpr uint32_t LINK_RETRY_AFTER_MS = 20 * 1000;
constexpr uint32_t LINK_RETRY_INTERVAL_MS = 30 * 1000;

// Network-stack watchdog glue (fw 1.11.0, ledger shq-suite-0044; the policy itself is the pure
// netwatch::Policy). One ICMP echo to the DHCP gateway every NET_PROBE_INTERVAL_MS via the IDF's
// esp_ping (a short-lived task per probe; the session frees itself in on_ping_end). The probe is
// deliberately the GATEWAY and not HA: an HA outage must never look like a dead stack. A probe
// whose callbacks never arrive is counted as failed after NET_PROBE_GIVEUP_MS — under the heap
// starvation this exists to catch, esp_ping_new_session itself can fail, which is the same
// verdict. The policy ticks at 1 Hz; ESP.getFreeHeap() takes a heap lock, so no faster.
constexpr uint32_t NET_PROBE_INTERVAL_MS = 60 * 1000;
constexpr uint32_t NET_PROBE_TIMEOUT_MS = 2000;
constexpr uint32_t NET_PROBE_GIVEUP_MS = 10 * 1000;
constexpr uint32_t NET_TICK_MS = 1000;

Status g_status = Status::PORTAL;
char g_hostname[24] = "somfy-sdn";
WinkCb g_wink_cb = nullptr;

// Portal-mode servers (only created when no STA connection).
WebServer* g_portal = nullptr;
DNSServer* g_dns = nullptr;

// Button state.
bool g_btn_down = false;
uint32_t g_btn_down_ms = 0;

// Set by requestReconnectBestAp() (HTTP/WS handler context) and serviced from loop(), so the
// HTTP/WS ack flushes before we drop the link to re-scan.
bool g_reconnect_requested = false;

// Watchdog state.
uint32_t g_wifi_down_since_ms = 0;   // mono::now() when the STA link was first seen down; 0 = up
uint32_t g_portal_started_ms = 0;    // mono::now() when the portal started
bool g_portal_has_creds = false;     // creds exist => portal retry applies
char g_boot_note[24] = "none";       // previous boot's noteReboot() reason
uint32_t g_last_link_retry_ms = 0;   // last forced re-begin attempt; 0 = none this outage
String g_ssid, g_pass;               // cached creds for the retry loop (avoid NVS reads in loop)

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

// WiFi event telemetry (fw 1.5.0). Set from the WiFi event task — keep the handler minimal
// (counters only, no Serial/heap work); loop() does the logging and mDNS servicing.
volatile uint32_t g_wifi_disc_count = 0;   // lifetime STA disconnect events since boot
volatile uint8_t g_wifi_last_reason = 0;   // last disconnect reason code (802.11 reason)
volatile bool g_got_ip_event = false;      // GOT_IP seen — service mDNS re-announce from loop()
uint32_t g_wifi_disc_logged = 0;           // last count logged from loop()

// Network-stack watchdog state (fw 1.11.0). The ping callbacks run on the ping task; they only
// write g_probe_result, which the 1 Hz tick in loop() consumes.
netwatch::Policy g_netwatch(true);
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

void onWiFiEvent(WiFiEvent_t event, WiFiEventInfo_t info) {
  switch (event) {
    case ARDUINO_EVENT_WIFI_STA_DISCONNECTED:
      g_wifi_disc_count = g_wifi_disc_count + 1;
      g_wifi_last_reason = info.wifi_sta_disconnected.reason;
      break;
    case ARDUINO_EVENT_WIFI_STA_GOT_IP:
      g_got_ip_event = true;
      break;
    default:
      break;
  }
}

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
  // Pick the STRONGEST AP for the SSID, not the first one found. The Arduino default is
  // WIFI_FAST_SCAN, which associates to the first matching BSSID that clears the RSSI threshold
  // (often the previously-cached AP), so a device near a strong AP but cached onto a distant one
  // stays stuck on the distant one across reboots. All-channel scan + sort-by-signal makes every
  // (re)connect evaluate all APs and join the strongest — so a reboot/manual reconnect actually
  // moves to the nearest AP. The Arduino WiFi stack has no live roaming once associated; the WS
  // `reconnect_wifi` admin command / HTTP `POST /reconnect` force a re-evaluation on demand.
  WiFi.setScanMethod(WIFI_ALL_CHANNEL_SCAN);
  WiFi.setSortMethod(WIFI_CONNECT_AP_BY_SIGNAL);
  applyProto();  // fw 1.13.0: after mode() (esp_wifi_init done), before begin()
  for (uint8_t attempt = 0; attempt <= STA_RETRIES; attempt++) {
    Serial.printf("# WiFi connecting to \"%s\" (attempt %d)", ssid.c_str(), attempt + 1);
    WiFi.begin(ssid.c_str(), pass.c_str());
    uint32_t t0 = mono::now();
    while (!WiFi.isConnected() && mono::now() - t0 < STA_CONNECT_TIMEOUT_MS) {
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
  g_portal_started_ms = mono::now();
  {
    String s, p;
    g_portal_has_creds = readCreds(s, p);
  }
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
  uint32_t now = mono::now();
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

  // Recover the previous boot's reboot note (if any), then clear it so a subsequent power
  // cycle reads "none" — the note describes THIS boot's cause only.
  {
    Preferences p;
    if (p.begin(NVS_NS, false)) {
      String note = p.getString(NVS_BOOTNOTE, "");
      if (note.length() > 0) {
        strncpy(g_boot_note, note.c_str(), sizeof(g_boot_note) - 1);
        g_boot_note[sizeof(g_boot_note) - 1] = '\0';
        p.remove(NVS_BOOTNOTE);
        Serial.printf("# boot note: %s\n", g_boot_note);
      }
      p.end();
    }
  }

  // Register the event hook before the first connect so boot-time disconnects are counted too.
  WiFi.onEvent(onWiFiEvent);

  readProto();  // fw 1.13.0 WiFi protocol A/B knob; tryConnect applies it

  String ssid, pass;
  if (readCreds(ssid, pass) && tryConnect(ssid, pass)) {
    g_ssid = ssid;  // cached for the link-retry loop
    g_pass = pass;
    g_status = Status::CONNECTED;
    Serial.printf("# WiFi up: http://%s/  (%s)\n", WiFi.localIP().toString().c_str(),
                  g_hostname);
    startMdns();
    g_got_ip_event = false;  // boot GOT_IP already handled by the startMdns() above
  } else {
    g_status = Status::PORTAL;
    startPortal();
  }
  return g_status;
}

// Force a fresh all-channel scan and reassociate to the strongest AP. Runs from loop() (not the
// HTTP/WS handler) so the ack has flushed first — dropping the link mid-response would lose it.
// Reconnecting on the same L2 network keeps the DHCP lease (IP unchanged), so mDNS stays valid.
// Drop the association and rejoin. Shared by the manual reconnect and the network-stack
// watchdog: this is the exact operation that brought Bed 2 back from the dead on 2026-09-04
// (a router-side kick) — the WiFi driver tears down and re-arms its timers against the current
// clock and frees what it had queued (heap 8 k -> 240 k in the same second). tryConnect blocks
// the main loop for a few seconds; the bus task keeps running. If it fails the link watchdog
// takes over (link-retry, then wifi-dead reboot).
static void reassociate(const char* why) {
  if (g_status != Status::CONNECTED) return;  // only meaningful in STA mode
  String ssid, pass;
  if (!readCreds(ssid, pass)) return;
  Serial.printf("# WiFi: reassociating (%s; was rssi=%d heap=%u) — re-scanning\n", why,
                (int)WiFi.RSSI(), (unsigned)ESP.getFreeHeap());
  delay(250);  // let the in-flight HTTP/WS ack reach the wire before the link drops
  WiFi.disconnect();
  delay(100);
  tryConnect(ssid, pass);  // all-channel scan + sort-by-signal -> strongest BSSID
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
  switch (v.action) {
    case netwatch::Action::Reassociate:
      Serial.printf("# netwatch: %s — reassociating (fails=%u heap=%u)\n", v.reason,
                    (unsigned)g_netwatch.consecutiveFailures(), (unsigned)in.heap);
      diag::noteNetRecover(v.reason, g_netwatch.recoveries());
      reassociate(v.reason);
      // The probe in flight (if any) belongs to the old association; start afresh.
      g_probe_inflight = false;
      g_probe_result = 0;
      g_last_probe_ms = 0;
      break;
    case netwatch::Action::Reboot:
      Serial.printf("# netwatch: %s persisted through a reassociation — rebooting\n", v.reason);
      diag::noteNetRecover(v.reason, g_netwatch.recoveries());
      noteReboot(v.reason);
      break;
    default:
      break;
  }
}

// Log WiFi events + re-announce mDNS from the main loop (the event handler itself stays minimal).
// The mDNS restart on every (re)association matters after an infra outage: the device may come
// back on a NEW IP, and HA's zeroconf host-healing only works if the advert actually re-fires —
// ESPmDNS's own behaviour on IP change is not dependable. A fresh end()+begin() re-announces,
// letting HA rewrite the stored host instead of dialling a stale IP forever.
void serviceWifiEvents() {
  if (g_wifi_disc_logged != g_wifi_disc_count) {
    g_wifi_disc_logged = g_wifi_disc_count;
    Serial.printf("# WiFi: STA disconnect #%u (reason=%u)\n", (unsigned)g_wifi_disc_logged,
                  (unsigned)g_wifi_last_reason);
  }
  if (g_got_ip_event) {
    g_got_ip_event = false;
    if (g_status == Status::CONNECTED) {
      Serial.printf("# WiFi: got IP %s (rssi=%d) — re-announcing mDNS\n",
                    WiFi.localIP().toString().c_str(), (int)WiFi.RSSI());
      MDNS.end();
      startMdns();
    }
  }
}

// WiFi-death watchdog + link-retry loop + portal-purgatory retry (constants up top for rationale).
void serviceWatchdogs() {
  uint32_t now = mono::now();
  if (g_status == Status::CONNECTED) {
    if (WiFi.isConnected()) {
      g_wifi_down_since_ms = 0;
      g_last_link_retry_ms = 0;
      return;
    }
    if (g_wifi_down_since_ms == 0) {
      g_wifi_down_since_ms = now;
      Serial.println("# WiFi: STA link down — watchdog armed");
      return;
    }
    uint32_t down_for = (uint32_t)(now - g_wifi_down_since_ms);
    if (down_for >= WIFI_DEAD_REBOOT_MS) {
      Serial.printf("# WiFi: STA link dead for >%us — rebooting to self-heal\n",
                    WIFI_DEAD_REBOOT_MS / 1000);
      noteReboot("wifi-dead");
    }
    // Active retry: give implicit auto-reconnect LINK_RETRY_AFTER_MS to win the easy cases, then
    // force a full re-begin (resets a stuck association state machine; re-applies the
    // all-channel strongest-AP scan). Non-blocking — connection progress is observed on
    // subsequent loop passes; the reboot above remains the backstop.
    if (down_for >= LINK_RETRY_AFTER_MS && g_ssid.length() > 0 &&
        (g_last_link_retry_ms == 0 ||
         (uint32_t)(now - g_last_link_retry_ms) >= LINK_RETRY_INTERVAL_MS)) {
      g_last_link_retry_ms = now;
      Serial.printf("# WiFi: link down %us (disc=%u last_reason=%u) — forcing re-begin\n",
                    down_for / 1000, (unsigned)g_wifi_disc_count, (unsigned)g_wifi_last_reason);
      WiFi.disconnect();
      WiFi.setScanMethod(WIFI_ALL_CHANNEL_SCAN);
      WiFi.setSortMethod(WIFI_CONNECT_AP_BY_SIGNAL);
      applyProto();  // idempotent; keeps the A/B setting on this re-begin path too
      WiFi.begin(g_ssid.c_str(), g_pass.c_str());
    }
  } else if (g_portal_has_creds &&
             (uint32_t)(now - g_portal_started_ms) >= PORTAL_RETRY_MS) {
    Serial.println("# portal: creds present, retry window elapsed — rebooting to retry STA");
    noteReboot("portal-retry");
  }
}

void loop() {
  serviceButton();
  serviceReconnect();
  serviceWifiEvents();
  serviceWatchdogs();
  serviceNetwatch();
  if (g_status == Status::PORTAL) {
    if (g_dns) g_dns->processNextRequest();
    if (g_portal) g_portal->handleClient();
  }
}

void requestReconnectBestAp() { g_reconnect_requested = true; }

void noteReboot(const char* reason) {
  Preferences p;
  if (p.begin(NVS_NS, false)) {
    p.putString(NVS_BOOTNOTE, reason);
    p.end();
  }
  Serial.flush();
  delay(100);
  ESP.restart();
  while (true) {}  // unreachable — satisfies [[noreturn]]
}

const char* bootNote() { return g_boot_note; }

uint32_t staDisconnectCount() { return g_wifi_disc_count; }
uint8_t lastDisconnectReason() { return g_wifi_last_reason; }

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
