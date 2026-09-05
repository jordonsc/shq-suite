// Somfy SDN controller for the Unexpected Maker TinyC6 (ESP32-C6) — a bus participant (NOT a
// MITM) on the Somfy Digital Network RS485 bus. Design twin of actron-sniffer. See SPEC.md.
//
// Boot: bring up the SDN bus task (LISTEN by default — firmware-gated TX safety), then WiFi.
// With WiFi up, start the HTTP debug API (port 80), the WS controller API (port 8767), and
// load any configured motor addresses from NVS. Without WiFi, a SoftAP captive portal runs for
// provisioning (no baked-in credentials). The GPIO0 button long-press wipes creds; short-press
// winks all detected motors.
//
// Wiring (SPEC §4): UART1 GPIO16 (TX/DI) / GPIO17 (RX/RO) -> 3.3 V auto-direction RS485
// transceiver -> A/B tapped in parallel onto the multi-drop SDN bus (no 120 ohm terminator on a
// mid-bus tap). GPIO0 momentary button -> GND.

#include <Arduino.h>
#include <ArduinoOTA.h>

#include <cstring>

#include "bus.h"
#include "devices.h"
#include "diag.h"
#include "fault.h"
#include "mono.h"
#include "http_api.h"
#include "version.h"
#include "wifi_proto.h"
#include "wifi_prov.h"
#include "ws_api.h"

static void onStateChanged() { ws_api::notifyStateChanged(); }

// ---- USB serial console --------------------------------------------------
// Bench / field debug over the USB-CDC console — drives the SDN bus without needing WiFi
// (works in provisioning mode too). Mirrors the HTTP debug API. Line-based; type "help".

static int consoleHexNibble(char c) {
  if (c >= '0' && c <= '9') return c - '0';
  if (c >= 'a' && c <= 'f') return c - 'a' + 10;
  if (c >= 'A' && c <= 'F') return c - 'A' + 10;
  return -1;
}

static void printReply(const bus::RawResult& res) {
  if (!res.sent) {
    Serial.println("# NOT SENT: bus in LISTEN mode — run 'mode active' first");
    return;
  }
  if (!res.got_reply) {
    Serial.println("# sent, no reply (timeout)");
    return;
  }
  Serial.print("# reply:");
  for (size_t i = 0; i < res.raw_len; i++) Serial.printf(" %02X", res.raw[i]);
  Serial.printf("\n# parsed msg_id=0x%02X data_len=%u data=", res.reply.msg_id,
                (unsigned)res.reply.data_len);
  for (size_t i = 0; i < res.reply.data_len; i++) Serial.printf("%02X ", res.reply.data[i]);
  Serial.println();
}

static void printDevices() {
  devices::DeviceTable& t = bus::table();
  Serial.printf("# %u device(s):\n", (unsigned)t.count());
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (!d) continue;
    char a[9];
    sdn::formatAddress(d->addr, a);
    Serial.printf("  %s pos=%s%% pulses=%u up=%u down=%u dir=%s move=%d fault=%d online=%d src=%d\n",
                  a, d->position_known ? String(d->position_pct).c_str() : "?",
                  d->position_pulses, d->up_limit_pulses, d->down_limit_pulses,
                  d->direction_known ? (d->direction ? "rev" : "std") : "?",
                  (int)d->movement, d->fault, d->online, (int)d->source);
  }
}

static void printStatus() {
  const bus::Stats& s = bus::stats();
  Serial.printf("# mode=%s devices=%u tx=%u rx=%u polls=%u err[cksum=%u framing=%u timeout=%u nack=%u total=%u] fw=\"%s\"\n",
                bus::mode() == bus::Mode::ACTIVE ? "active" : "listen",
                (unsigned)bus::table().count(), s.tx_frames, s.rx_frames, s.polls,
                s.err_checksum, s.err_framing, s.err_timeout, s.err_nack,
                (unsigned)bus::errors().total(),
                SOMFY_FW_VERSION " (" __DATE__ " " __TIME__ ")");
}

static void doSend(uint8_t msg, char* addr_tok, char** data_toks, int n_data) {
  uint8_t addr[3];
  bool bc = (addr_tok == nullptr) || strcmp(addr_tok, "bc") == 0;
  if (!bc && !sdn::parseAddress(addr_tok, addr)) {
    Serial.println("# bad addr (want AA:BB:CC or 'bc')");
    return;
  }
  uint8_t data[sdn::MAX_FRAME_LEN];
  int dlen = 0;
  for (int i = 0; i < n_data && dlen < (int)sizeof(data); i++) {
    data[dlen++] = (uint8_t)strtoul(data_toks[i], nullptr, 16);
  }
  bus::RawResult res;
  if (!bus::requestRaw(bc ? nullptr : addr, bc, msg, data, dlen, &res, 8000)) {
    Serial.println("# /send failed (queue/timeout)");
    return;
  }
  printReply(res);
}

static void pumpConsole() {
  static char buf[160];
  static size_t len = 0;
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\r') continue;
    if (c != '\n') {
      if (len < sizeof(buf) - 1) buf[len++] = c;
      continue;
    }
    buf[len] = '\0';
    len = 0;

    // Tokenise.
    char* toks[24];
    int n = 0;
    char* p = strtok(buf, " ");
    while (p && n < 24) { toks[n++] = p; p = strtok(nullptr, " "); }
    if (n == 0) continue;
    const char* cmd = toks[0];

    if (!strcmp(cmd, "help")) {
      Serial.println("# commands:");
      Serial.println("#  s | d | mode active|listen | disc | errs | clear");
      Serial.println("#  send <addr|bc> <msghex> [databytes...]   raw frame, prints reply");
      Serial.println("#  get <addr> pos|lim|dir|status            convenience reads");
      Serial.println("#  move <addr> open|close|stop|pos [ha%]    (needs limits set)");
      Serial.println("#  jog <addr> up|down [dur]                 CTRL_MOVE timed nudge (commissioning)");
      Serial.println("#  setbottom <addr> <pulses>                set bottom limit at absolute pulses");
      Serial.println("#  forget <addr>                            remove motor from table");
      Serial.println("#  cfg <baud> <n|e|o>                       re-init UART (bring-up)");
      Serial.println("#  probe [ms]                               TX bcast GET_NODE_ADDR, dump raw RX");
      Serial.println("#  proto [bgnax|bgn|bg]                     WiFi protocol A/B knob (persists; REBOOTS to apply)");
    } else if (!strcmp(cmd, "cfg") && n >= 3) {
      uint32_t baud = strtoul(toks[1], nullptr, 10);
      bus::reconfigure(baud, toks[2][0]);
      delay(100);
      Serial.printf("# UART -> %u baud 8%c1\n", (unsigned)bus::currentBaud(), bus::currentParity());
    } else if (!strcmp(cmd, "probe")) {
      uint32_t ms = (n >= 2) ? strtoul(toks[1], nullptr, 10) : 400;
      uint8_t raw[64];
      size_t k = bus::rawProbe(raw, sizeof(raw), ms);
      Serial.printf("# probe @ %u baud 8%c1: %u raw byte(s) back:", (unsigned)bus::currentBaud(),
                    bus::currentParity(), (unsigned)k);
      for (size_t i = 0; i < k; i++) Serial.printf(" %02X", raw[i]);
      Serial.println();
    } else if (!strcmp(cmd, "proto")) {
      // fw 1.13.0 WiFi protocol A/B knob — same semantics as POST /wifiproto.
      if (n < 2) {
        Serial.printf("# wifi_proto=%s\n", wifi_proto::name(wifi_prov::wifiProto()));
      } else {
        wifi_proto::Proto proto;
        if (!wifi_proto::parse(toks[1], &proto)) {
          Serial.println("# proto must be bgnax|bgn|bg");
        } else if (!wifi_prov::setWifiProto(proto)) {
          Serial.println("# nvs write failed");
        } else {
          Serial.printf("# wifi_proto=%s — rebooting to apply\n", wifi_proto::name(proto));
          Serial.flush();
          delay(100);
          wifi_prov::noteReboot("wifiproto");
        }
      }
    } else if (!strcmp(cmd, "s")) {
      printStatus();
    } else if (!strcmp(cmd, "d")) {
      printDevices();
    } else if (!strcmp(cmd, "mode") && n >= 2) {
      bus::setMode(!strcmp(toks[1], "active") ? bus::Mode::ACTIVE : bus::Mode::LISTEN);
      printStatus();
    } else if (!strcmp(cmd, "clear")) {
      bus::clearDiagnostics();
      Serial.println("# cleared");
    } else if (!strcmp(cmd, "disc")) {
      bus::Command jc;
      jc.type = bus::CmdType::REDISCOVER;
      jc.broadcast = true;
      jc.job_id = bus::nextJobId();
      bus::enqueue(jc);
      Serial.println("# discovery queued");
    } else if (!strcmp(cmd, "errs")) {
      static errlog::Entry e[errlog::RING_SIZE];
      size_t k = bus::errors().newest(e, errlog::RING_SIZE);
      Serial.printf("# %u error(s) (newest first):\n", (unsigned)k);
      for (size_t i = 0; i < k; i++) {
        char a[9] = "--";
        if (e[i].has_addr) sdn::formatAddress(e[i].addr, a);
        Serial.printf("  %u %.3f %s %s %s\n", (unsigned)e[i].seq, e[i].t_ms / 1000.0,
                      errlog::className(e[i].cls), a, e[i].msg);
      }
    } else if (!strcmp(cmd, "send") && n >= 3) {
      uint8_t msg = (uint8_t)strtoul(toks[2], nullptr, 16);
      doSend(msg, toks[1], &toks[3], n - 3);
    } else if (!strcmp(cmd, "get") && n >= 3) {
      uint8_t msg = 0;
      if (!strcmp(toks[2], "pos")) msg = sdn::MSG_GET_MOTOR_POSITION;
      else if (!strcmp(toks[2], "lim")) msg = sdn::MSG_GET_MOTOR_LIMITS;
      else if (!strcmp(toks[2], "dir")) msg = sdn::MSG_GET_MOTOR_DIRECTION;
      else if (!strcmp(toks[2], "status")) msg = sdn::MSG_GET_MOTOR_STATUS;
      else { Serial.println("# get <addr> pos|lim|dir|status"); continue; }
      doSend(msg, toks[1], nullptr, 0);
    } else if (!strcmp(cmd, "move") && n >= 3) {
      uint8_t addr[3];
      if (!sdn::parseAddress(toks[1], addr)) { Serial.println("# bad addr"); continue; }
      bus::Command jc;
      memcpy(jc.addr, addr, 3);
      jc.job_id = bus::nextJobId();
      if (!strcmp(toks[2], "open")) jc.type = bus::CmdType::OPEN;
      else if (!strcmp(toks[2], "close")) jc.type = bus::CmdType::CLOSE;
      else if (!strcmp(toks[2], "stop")) jc.type = bus::CmdType::STOP;
      else if (!strcmp(toks[2], "pos") && n >= 4) {
        jc.type = bus::CmdType::SET_POSITION;
        jc.u8 = sdn::haToSomfy((uint8_t)atoi(toks[3]));
      } else { Serial.println("# move <addr> open|close|stop|pos [ha%]"); continue; }
      bus::enqueue(jc);
      Serial.println("# queued");
    } else if (!strcmp(cmd, "jog") && n >= 3) {
      uint8_t addr[3];
      if (!sdn::parseAddress(toks[1], addr)) { Serial.println("# bad addr"); continue; }
      bool up = !strcmp(toks[2], "up");
      if (!up && strcmp(toks[2], "down")) { Serial.println("# jog <addr> up|down [dur]"); continue; }
      int dur = (n >= 4) ? atoi(toks[3]) : 20;
      if (dur < sdn::MOVE_DURATION_MIN) dur = sdn::MOVE_DURATION_MIN;
      if (dur > 255) dur = 255;
      bus::Command jc;
      memcpy(jc.addr, addr, 3);
      jc.type = bus::CmdType::MOVE_TIMED;
      jc.u8 = up ? 1 : 0;
      jc.u16 = (uint16_t)dur;
      jc.job_id = bus::nextJobId();
      bus::enqueue(jc);
      Serial.printf("# jog %s dur=%d queued\n", up ? "up" : "down", dur);
    } else if (!strcmp(cmd, "setbottom") && n >= 3) {
      uint8_t addr[3];
      if (!sdn::parseAddress(toks[1], addr)) { Serial.println("# bad addr"); continue; }
      int p = atoi(toks[2]);
      if (p < 0 || p > 65535) { Serial.println("# pulses 0..65535"); continue; }
      bus::Command jc;
      memcpy(jc.addr, addr, 3);
      jc.type = bus::CmdType::SET_BOTTOM_LIMIT_AT;
      jc.u16 = (uint16_t)p;
      jc.job_id = bus::nextJobId();
      bus::enqueue(jc);
      Serial.printf("# set bottom limit at %d pulses queued\n", p);
    } else if (!strcmp(cmd, "forget") && n >= 2) {
      uint8_t addr[3];
      if (!sdn::parseAddress(toks[1], addr)) { Serial.println("# bad addr"); continue; }
      bus::Command jc;
      memcpy(jc.addr, addr, 3);
      jc.type = bus::CmdType::FORGET;
      jc.job_id = bus::nextJobId();
      bus::enqueue(jc);
      Serial.println("# forget queued");
    } else {
      Serial.println("# ? type 'help'");
    }
  }
}

// GPIO0 short-press: wink every motor in the table (install-time "which blinds is this?").
// Only transmits if the bus is ACTIVE (firmware TX gate); harmless no-op in LISTEN.
static void winkAll() {
  devices::DeviceTable& t = bus::table();
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (d == nullptr) continue;
    bus::Command c;
    c.type = bus::CmdType::IDENTIFY;
    memcpy(c.addr, d->addr, 3);
    c.job_id = bus::nextJobId();
    bus::enqueue(c);
  }
}

void setup() {
  Serial.begin(115200);
  uint32_t t0 = millis();
  while (!Serial && millis() - t0 < 1500) delay(10);
  Serial.println("\n# Somfy SDN controller");

  // Bus task first — it runs in LISTEN regardless of WiFi, so we can sniff the wire even while
  // provisioning. TX stays gated until something arms ACTIVE.
  bus::begin(onStateChanged);
  wifi_prov::setWinkCallback(winkAll);

  wifi_prov::Status st = wifi_prov::begin();
  if (st == wifi_prov::Status::CONNECTED) {
    wifi_prov::loadConfiguredMotors();
    http_api::begin(80);
    ws_api::begin(8767);
    ArduinoOTA.setHostname(wifi_prov::hostname());
    ArduinoOTA.begin();
    // Diagnostics last, so its Boot record carries a settled picture of the machine (heap after
    // the servers have allocated, socket headroom with them already bound).
    diag::begin();
    Serial.printf("# ready: http://%s.local/  ws://%s.local:8767/\n", wifi_prov::hostname(),
                  wifi_prov::hostname());
  } else {
    Serial.println("# provisioning mode — connect to the SoftAP and configure WiFi");
  }
}


// ---- fault evaluation ----------------------------------------------------

// Free heap below this and a WS reconnect (TLS-free, but still a socket plus JSON buffers) starts
// failing. Well above the ~97 kB minimum this fleet has actually touched.
static constexpr uint32_t FAULT_HEAP_LOW_BYTES = 60000;
static constexpr uint32_t FAULT_HEAP_CLEAR_BYTES = 80000;

// Clock liveness, counted in LOOP ITERATIONS rather than milliseconds — timing the clock against
// itself would be circular, and the clock is the thing under suspicion (ledger shq-suite-0041).
// mono's re-baseline should make this unreachable; it is the backstop that proves it, and the
// only signal that would have named Bed 2's nine-hour wedge while it was happening. The loop runs
// at kHz, so a genuine millisecond tick arrives within a few dozen iterations; twenty thousand
// without one cannot happen on a working clock.
static constexpr uint32_t CLOCK_STALL_LOOPS = 20000;
static uint32_t g_clock_last_ms = 0;
static uint32_t g_clock_same_loops = 0;

// The clock check runs every pass (an integer compare); everything else is polled on a loop
// COUNT, not a timer, so the fault evaluator keeps working when the clock is the thing that
// broke — and so the hot path never pays for a heap query or a table walk per iteration.
static constexpr uint32_t FAULT_POLL_LOOPS = 512;
static uint32_t g_fault_poll = 0;
static constexpr uint32_t FAULT_HEAP_EVERY_N_POLLS = 8;
static uint32_t g_heap_poll = 0;

static void updateFaults(uint32_t now_ms) {
  char detail[fault::DETAIL_MAX];

  if (now_ms == g_clock_last_ms) {
    if (g_clock_same_loops < CLOCK_STALL_LOOPS) g_clock_same_loops++;
  } else {
    g_clock_last_ms = now_ms;
    g_clock_same_loops = 0;
    fault::clear(fault::Code::ClockStalled);
  }
  if (g_clock_same_loops >= CLOCK_STALL_LOOPS) {
    snprintf(detail, sizeof(detail), "pinned at %lu s", (unsigned long)(now_ms / 1000));
    fault::raise(fault::Code::ClockStalled, detail);
  }

  if (++g_fault_poll < FAULT_POLL_LOOPS) return;
  g_fault_poll = 0;

  // Latched, not live: both are historical facts about a clock that misbehaved and was handled.
  // They stay up until a reboot or POST /clear, because "it happened while you weren't looking"
  // is exactly the thing that went unreported for nine hours.
  if (mono::rebases() > 0) {
    snprintf(detail, sizeof(detail), "%lu x, last %ld ms", (unsigned long)mono::rebases(),
             (long)mono::lastRebaseMs());
    fault::raise(fault::Code::ClockRebase, detail);
  }
  if (mono::wordSteps() > 0) {
    snprintf(detail, sizeof(detail), "%lu x, last %u high-word units",
             (unsigned long)mono::wordSteps(), (unsigned)mono::lastWordUnits());
    fault::raise(fault::Code::ClockWordStep, detail);
  }

  // A controller with motors configured and none answering is not doing its job, whatever the
  // network thinks of it. Motors go offline on their own sweep timer, so this needs no debounce.
  devices::DeviceTable& t = bus::table();
  size_t online = 0;
  for (size_t i = 0; i < t.count(); i++) {
    devices::Device* d = t.at(i);
    if (d != nullptr && d->online) online++;
  }
  if (t.count() > 0 && online == 0) {
    snprintf(detail, sizeof(detail), "0/%u motors answering", (unsigned)t.count());
    fault::raise(fault::Code::BusOffline, detail);
  } else {
    fault::clear(fault::Code::BusOffline);
  }

  // Heap is sub-sampled again on top of the 512-loop poll. `ESP.getFreeHeap()` takes a HEAP LOCK,
  // which is why diag::tick() already rate-limits its own read to 1 Hz; the main loop runs at
  // ~1 kHz, so polling heap every 512 iterations would take that lock at ~2 Hz — twice diag's
  // deliberate limit, on hardware whose RS485 relay is timing-critical. One read per 8 polls is
  // ~0.25 Hz, comfortably below it, and heap simply does not move fast enough to care.
  if (++g_heap_poll >= FAULT_HEAP_EVERY_N_POLLS) {
    g_heap_poll = 0;
    const uint32_t heap = ESP.getFreeHeap();
    if (heap < FAULT_HEAP_LOW_BYTES) {
      snprintf(detail, sizeof(detail), "%lu bytes free", (unsigned long)heap);
      fault::raise(fault::Code::HeapLow, detail);
    } else if (heap > FAULT_HEAP_CLEAR_BYTES) {
      fault::clear(fault::Code::HeapLow);
    }
  }
}

void loop() {
  // Phase timing (ledger shq-suite-0038). A WS client evicted for a late pong is indistinguishable
  // from a dead one unless you know whether THIS loop was stalled at the time — and knowing which
  // phase stalled separates "a slow HTTP peer blocked the pump" from a fault in the WS layer
  // itself. Costs four clock reads per iteration.
  const uint32_t t_start = mono::now();
  uint32_t ota_ms = 0, http_ms = 0, ws_ms = 0;

  pumpConsole();      // USB serial debug console (works with or without WiFi)
  wifi_prov::loop();  // button (both modes) + captive portal (PORTAL mode)
  if (wifi_prov::isConnected()) {
    const uint32_t t_ota = mono::now();
    ArduinoOTA.handle();
    const uint32_t t_http = mono::now();
    http_api::loop();
    const uint32_t t_ws = mono::now();
    ws_api::loop();
    const uint32_t t_end = mono::now();
    ota_ms = t_http - t_ota;
    http_ms = t_ws - t_http;
    ws_ms = t_end - t_ws;
  }
  updateFaults(t_start);
  diag::noteLoop((uint32_t)(mono::now() - t_start), ota_ms, http_ms, ws_ms);
  // The SDN bus runs on its own FreeRTOS task (bus::begin) — see bus.cpp.
}
