// Actron NEO <-> indoor-unit RS485 tool for the Unexpected Maker TinyC6 (ESP32-C6).
//
// The bus is Modbus RTU 9600/8N1; the indoor board is master, wall controllers are slaves at
// 0x66 (the NEO) / 0x67 / 0x68. This captures framed hex into a RAM ring buffer over an open
// (LAN-only) HTTP server, AND can emulate a controller to WRITE: answer the board's poll for a
// slot and either report register overrides or fire a one-shot command pulse (e.g. reg14=4 =
// setpoint). Local write control of mode/fan/main-setpoint is proven via a 0x67 command pulse
// (the NEO stays live). See FINDINGS.md. Default state is disarmed/receive-only.
//
// HTTP (port 80):
//   GET /            help + status
//   GET /stats       one-line status (seq_max, armed, addr, tmpl1/2, poll/tx)
//   GET /log         recent frames; ?since=<seq> for incremental, ?n=<max>
//   GET /set         ?baud=&parity=N|E|O&gap=<us>   change capture settings (use gap=5000)
//   GET /measure     estimate baud from raw line pulse widths (~5s)
//   GET /clear       reset ring + counters
//   GET /armwrite    ?addr=&ovr=reg:val,...&pulse=reg:val&pulsen=N&turn=us   ARM emulation/write
//   GET /disarm      back to receive-only
//   GET /txprobe     TX self-test (inject a poll to 0x66, report if it answers)
//   GET /update      ?url=<bin>   HTTP-pull OTA
//
// Wiring: auto-direction TTL<->RS485 module (no DE/RE — it keys the driver off UART TX).
//   module RXD/DI <- GPIO16 (board "TX")    module TXD/RO -> GPIO17 (board "RX")    + 3V3/GND
//   A=RJ45 pin4 (blue), B=RJ45 pin5 (white/blue), GND ref=pin7 (white/brown).
//   2-wire shared bus, so our TX reaches both NEO and board. Receive-only is enforced in firmware
//   (never write to Bus) unless armed. (Nuance: GPIO16 may carry the ROM boot-log briefly at
//   power-on; harmless — Modbus tolerates a stray bad frame.)

#include <Arduino.h>
#include <WiFi.h>
#include <WebServer.h>
#include <ESPmDNS.h>
#include <ArduinoOTA.h>
#include <HTTPUpdate.h>

// ---- WiFi (baked in — throwaway experiment, LAN only) --------------------
static const char *WIFI_SSID = "SHQ";
static const char *WIFI_PASS = "REDACTED-WIFI-PASS";
static const char *HOSTNAME = "actron-sniffer";  // -> http://redacted.local/

// ---- pins -----------------------------------------------------------------
static const uint8_t PIN_RS485_RX = 17;  // module TXD/RO -> board "RX" (UART1 RX via GPIO matrix)
static const uint8_t PIN_RS485_TX = 16;  // module RXD/DI <- board "TX" (UART1 TX); auto-direction

HardwareSerial &Bus = Serial1;  // UART1; Serial (USB CDC) stays as a fallback console
WebServer server(80);

// ---- live-configurable capture settings ----------------------------------
static uint32_t g_baud = 9600;
static char g_parity = 'N';        // N | E | O
static uint32_t g_gap_us = 3000;   // idle gap that ends a frame
static bool g_capture = true;

// ---- stats ----------------------------------------------------------------
static uint64_t g_total_bytes = 0;
static uint32_t g_total_frames = 0;
static volatile uint32_t g_rx_errors = 0;

// ---- frame ring buffer ----------------------------------------------------
static const size_t FRAME_MAX = 512;  // capture whole messages (~253 B) — no 128 B splitting
static const size_t RING = 128;
struct FrameRec {
  uint32_t seq;     // monotonic, 1-based; 0 = empty slot
  uint32_t t_ms;    // capture time (ms since boot)
  uint32_t gap_us;  // idle gap before this frame
  uint16_t len;
  uint8_t data[FRAME_MAX];
};
static FrameRec g_ring[RING];
static uint32_t g_seq = 0;  // last assigned sequence number

// ---- current-frame assembly ----------------------------------------------
static uint8_t g_frame[FRAME_MAX];
static size_t g_flen = 0;
static uint32_t g_first_byte_us = 0;
static uint32_t g_last_byte_us = 0;
static uint32_t g_prev_frame_end_us = 0;

// ---- write-test: 0x67 controller emulation (OFF by default) ---------------
// When ARMED, answer the indoor board's `67 03 00 02 00 7C` page-1 poll (read regs 2-125)
// with a forged-but-valid Modbus response = the last cached 0x66 page-1 dump, with chosen
// registers overridden. Tests whether the board honours an un-commissioned controller slot.
// Stays receive-only until /armwrite is called; only ever answers a CRC-valid 0x67 page-1 poll.
static bool g_write_armed = false;
static uint8_t g_emul_addr = 0x67; // which controller slot we emulate (0x66/0x67/0x68)
static uint8_t g_page1[248];     // regs 2..125   from the last good 66 03 F8 response
static uint8_t g_page2[244];     // regs 126..247 from the last good 66 03 F4 response
static bool g_page1_valid = false;
static bool g_page2_valid = false;
static const int OVR_MAX = 8;
static uint16_t g_ovr_reg[OVR_MAX];
static uint16_t g_ovr_val[OVR_MAX];
static int g_ovr_n = 0;
static uint32_t g_poll67 = 0;    // polls (to g_emul_addr) seen while armed
static uint32_t g_tx67 = 0;      // responses transmitted
static uint32_t g_turn_us = 5000; // turnaround before our reply (must exceed Modbus t3.5 ~3.65ms @9600)
static uint16_t g_pulse_reg = 0;  // transient command pulse (e.g. reg14=4): applied to the next
static uint16_t g_pulse_val = 0;  // g_pulse_n page-1 responses only, then reverts to the template
static int g_pulse_n = 0;         // remaining page-1 responses to apply the pulse

static uint16_t modbusCrc(const uint8_t *d, size_t n) {
  uint16_t c = 0xFFFF;
  for (size_t i = 0; i < n; i++) {
    c ^= d[i];
    for (int k = 0; k < 8; k++) c = (c & 1) ? ((c >> 1) ^ 0xA001) : (c >> 1);
  }
  return c;  // appended to the wire low byte first
}

// Cache the latest complete, CRC-valid 0x66 responses as our response templates.
// page1 = 66 03 F8 (248 data, 253 total); page2 = 66 03 F4 (244 data, 249 total).
static void maybeCacheTemplates(const uint8_t *f, size_t len) {
  if (f[0] != 0x66 || f[1] != 0x03) return;
  if (len == 253 && f[2] == 0xF8 && modbusCrc(f, 251) == (uint16_t)(f[251] | (f[252] << 8))) {
    memcpy(g_page1, f + 3, 248); g_page1_valid = true;
  } else if (len == 249 && f[2] == 0xF4 && modbusCrc(f, 247) == (uint16_t)(f[247] | (f[248] << 8))) {
    memcpy(g_page2, f + 3, 244); g_page2_valid = true;
  }
}

static uint32_t parityConfig() {
  switch (g_parity) {
    case 'E': return SERIAL_8E1;
    case 'O': return SERIAL_8O1;
    default:  return SERIAL_8N1;
  }
}

static void startBus() {
  Bus.end();
  Bus.setRxBufferSize(4096);  // tolerate HTTP-handler latency without dropping bytes
  Bus.begin(g_baud, parityConfig(), PIN_RS485_RX, PIN_RS485_TX);
  Bus.onReceiveError([](hardwareSerial_error_t e) {
    if (e == UART_FRAME_ERROR || e == UART_PARITY_ERROR)
      g_rx_errors = g_rx_errors + 1;
  });
}

static void ledActivity() {
#ifdef RGB_BUILTIN
  static bool on = false;
  on = !on;
  rgbLedWrite(RGB_BUILTIN, 0, on ? 6 : 0, 0);  // dim green wink per frame
#endif
}

static size_t formatFrame(const FrameRec &f, char *out, size_t cap) {
  size_t n = snprintf(out, cap, "%u %.6f +%uus %u:",
                      f.seq, f.t_ms / 1000.0, f.gap_us, f.len);
  for (size_t i = 0; i < f.len && n + 4 < cap; i++)
    n += snprintf(out + n, cap - n, " %02X", f.data[i]);
  if (n + 2 < cap) { out[n++] = ' '; out[n++] = '|'; }
  for (size_t i = 0; i < f.len && n + 2 < cap; i++) {
    char c = (char)f.data[i];
    out[n++] = (c >= 32 && c < 127) ? c : '.';
  }
  if (n + 2 < cap) { out[n++] = '|'; out[n++] = '\n'; }
  out[n] = '\0';
  return n;
}

static void flushFrame() {
  if (g_flen == 0) return;
  uint32_t s = ++g_seq;
  FrameRec &r = g_ring[(s - 1) % RING];
  r.seq = s;
  r.t_ms = g_first_byte_us / 1000;
  r.gap_us = g_prev_frame_end_us ? (g_first_byte_us - g_prev_frame_end_us) : 0;
  r.len = (g_flen > FRAME_MAX) ? FRAME_MAX : g_flen;
  memcpy(r.data, g_frame, r.len);
  maybeCacheTemplates(r.data, r.len);

  g_total_frames++;
  g_prev_frame_end_us = g_last_byte_us;

  static char line[2300];
  formatFrame(r, line, sizeof(line));
  Serial.print(line);  // also echo to USB if anyone is watching

  g_flen = 0;
  ledActivity();
}

// Build + transmit a forged controller response for g_emul_addr. Values big-endian (high,low),
// matching the real 0x66 response. page2=false -> regs 2..125 (66 03 F8); true -> 126..247 (66 03 F4).
// Called the instant a CRC-valid poll to g_emul_addr is seen (while armed).
static void emitResponse(bool page2) {
  if (!g_write_armed) return;
  const uint8_t *tmpl;
  size_t ndata; uint16_t firstReg, lastReg; uint8_t bytecount;
  if (page2) {
    if (!g_page2_valid) return;
    tmpl = g_page2; ndata = 244; firstReg = 126; lastReg = 247; bytecount = 0xF4;
  } else {
    if (!g_page1_valid) return;
    tmpl = g_page1; ndata = 248; firstReg = 2; lastReg = 125; bytecount = 0xF8;
  }
  static uint8_t resp[253];
  resp[0] = g_emul_addr; resp[1] = 0x03; resp[2] = bytecount;
  memcpy(resp + 3, tmpl, ndata);
  for (int i = 0; i < g_ovr_n; i++) {
    uint16_t r = g_ovr_reg[i];
    if (r < firstReg || r > lastReg) continue;
    size_t off = 3 + (size_t)(r - firstReg) * 2;
    resp[off]     = (uint8_t)(g_ovr_val[i] >> 8);
    resp[off + 1] = (uint8_t)(g_ovr_val[i] & 0xFF);
  }
  // one-shot command pulse (page-1 only; reverts to template once g_pulse_n hits 0)
  if (!page2 && g_pulse_n > 0 && g_pulse_reg >= firstReg && g_pulse_reg <= lastReg) {
    size_t off = 3 + (size_t)(g_pulse_reg - firstReg) * 2;
    resp[off]     = (uint8_t)(g_pulse_val >> 8);
    resp[off + 1] = (uint8_t)(g_pulse_val & 0xFF);
    g_pulse_n--;
  }
  size_t total = 3 + ndata;             // bytes before CRC
  uint16_t c = modbusCrc(resp, total);
  resp[total]     = (uint8_t)(c & 0xFF);
  resp[total + 1] = (uint8_t)(c >> 8);
  delayMicroseconds(g_turn_us);         // turnaround after the master's poll (must exceed t3.5)
  Bus.write(resp, total + 2);
  Bus.flush();                          // block until fully shifted out
  g_tx67++;
  // We deliberately do NOT drain our own echo — it logs as a `<addr> 03 F8/F4` frame = TX proof.
}

static void pumpCapture() {
  uint32_t now = micros();
  if (g_flen > 0 && (now - g_last_byte_us) > g_gap_us) flushFrame();

  while (Bus.available()) {
    int b = Bus.read();
    if (b < 0) break;
    now = micros();
    if (g_flen > 0 && (now - g_last_byte_us) > g_gap_us) flushFrame();
    if (g_flen == 0) g_first_byte_us = now;
    if (g_flen < FRAME_MAX) g_frame[g_flen++] = (uint8_t)b;
    g_last_byte_us = now;
    g_total_bytes++;
    // Forge a reply the instant a CRC-valid poll to g_emul_addr completes (armed only).
    // page1 poll = `addr 03 00 02 00 7C`; page2 poll = `addr 03 00 7E 00 7A`.
    if (g_write_armed && g_flen == 8 &&
        g_frame[0] == g_emul_addr && g_frame[1] == 0x03 &&
        g_frame[2] == 0x00 && g_frame[4] == 0x00 &&
        modbusCrc(g_frame, 6) == (uint16_t)(g_frame[6] | (g_frame[7] << 8))) {
      bool p1 = (g_frame[3] == 0x02 && g_frame[5] == 0x7C);
      bool p2 = (g_frame[3] == 0x7E && g_frame[5] == 0x7A);
      if (p1 || p2) {
        g_poll67++;
        flushFrame();        // log the poll, reset assembly
        emitResponse(p2);    // forge + transmit
        continue;
      }
    }
    if (g_flen == FRAME_MAX) flushFrame();
  }
}

static size_t statusLine(char *out, size_t cap) {
  return snprintf(out, cap,
    "# seq_max=%u baud=%u parity=8%c1 gap=%uus capture=%s "
    "bytes=%llu frames=%u rx_err=%u rssi=%d ip=%s "
    "armed=%d addr=0x%02X tmpl1=%d tmpl2=%d poll=%u tx=%u fw=\"%s\"",
    g_seq, g_baud, g_parity, g_gap_us, g_capture ? "on" : "off",
    (unsigned long long)g_total_bytes, g_total_frames, g_rx_errors,
    WiFi.isConnected() ? WiFi.RSSI() : 0,
    WiFi.localIP().toString().c_str(),
    g_write_armed ? 1 : 0, g_emul_addr, g_page1_valid ? 1 : 0, g_page2_valid ? 1 : 0,
    g_poll67, g_tx67,
    __DATE__ " " __TIME__);
}

// ---- pulse-width baud estimator ------------------------------------------
// Times raw line edges; shortest pulse ~= one bit, so baud ~= 1e6 / min_us.
// Bus must be ACTIVE during the ~5s window (trigger a change in the Neo app).
static const size_t EDGE_CAP = 4096;
static volatile uint32_t g_edges[EDGE_CAP];
static volatile size_t g_edge_n = 0;

static void IRAM_ATTR edgeISR() {
  size_t n = g_edge_n;
  if (n < EDGE_CAP) { g_edges[n] = micros(); g_edge_n = n + 1; }
}

static uint32_t nearestStandardBaud(uint32_t b) {
  static const uint32_t std[] = {1200, 2400, 4800, 9600, 19200, 38400,
                                 57600, 76800, 115200};
  uint32_t best = std[0], bestErr = UINT32_MAX;
  for (uint32_t s : std) {
    uint32_t err = (s > b) ? (s - b) : (b - s);
    if (err < bestErr) { bestErr = err; best = s; }
  }
  return best;
}

static String measureBaud() {
  Bus.end();
  pinMode(PIN_RS485_RX, INPUT);
  g_edge_n = 0;
  attachInterrupt(digitalPinToInterrupt(PIN_RS485_RX), edgeISR, CHANGE);
  uint32_t deadline = millis() + 5000;
  while (millis() < deadline && g_edge_n < EDGE_CAP) delay(1);
  detachInterrupt(digitalPinToInterrupt(PIN_RS485_RX));

  size_t n = g_edge_n;
  String out;
  if (n < 8) {
    out = "# only " + String((unsigned)n) +
          " edges — idle/no traffic. Trigger a change and retry.\n";
    startBus();
    return out;
  }

  uint32_t mins[5] = {UINT32_MAX, UINT32_MAX, UINT32_MAX, UINT32_MAX, UINT32_MAX};
  for (size_t i = 1; i < n; i++) {
    uint32_t d = g_edges[i] - g_edges[i - 1];
    if (d < 3) continue;  // ignore glitches
    for (int k = 0; k < 5; k++) {
      if (d < mins[k]) {
        for (int j = 4; j > k; j--) mins[j] = mins[j - 1];
        mins[k] = d;
        break;
      }
    }
  }
  uint32_t bit_us = mins[0];
  uint32_t baud = bit_us ? (uint32_t)(1000000.0 / bit_us + 0.5) : 0;
  out = "# edges=" + String((unsigned)n) + " shortest_us=";
  for (int k = 0; k < 5; k++)
    if (mins[k] != UINT32_MAX) out += String(mins[k]) + " ";
  out += "\n# bit=" + String(bit_us) + "us -> ~" + String(baud) +
         " baud (nearest standard: " + String(nearestStandardBaud(baud)) + ")\n";
  startBus();
  return out;
}

// ---- HTTP handlers --------------------------------------------------------
static void handleRoot() {
  char st[300];
  statusLine(st, sizeof(st));
  String b = "Actron RS485 sniffer (listen-only)\n";
  b += String(st) + "\n\n";
  b += "GET /stats              status line\n";
  b += "GET /log?since=<seq>&n=<max>   frames (incremental)\n";
  b += "GET /set?baud=&parity=N|E|O&gap=<us>\n";
  b += "GET /measure            estimate baud from line pulses (~5s)\n";
  b += "GET /clear              reset ring + counters\n";
  b += "GET /armwrite?ovr=reg:val,...   ARM 0x67 write test (e.g. ovr=11:0x5903)\n";
  b += "GET /disarm             stop writing (receive-only)\n";
  server.send(200, "text/plain", b);
}

static void handleStats() {
  char st[300];
  statusLine(st, sizeof(st));
  server.send(200, "text/plain", String(st) + "\n");
}

static void handleLog() {
  uint32_t since = server.hasArg("since")
                       ? strtoul(server.arg("since").c_str(), nullptr, 10) : 0;
  uint32_t limit = server.hasArg("n")
                       ? strtoul(server.arg("n").c_str(), nullptr, 10) : 1000;

  server.setContentLength(CONTENT_LENGTH_UNKNOWN);
  server.send(200, "text/plain", "");
  char st[300];
  statusLine(st, sizeof(st));
  server.sendContent(String(st) + "\n");

  uint32_t maxseq = g_seq;
  uint32_t oldest = (maxseq > RING) ? (maxseq - RING + 1) : 1;
  uint32_t start = since + 1;
  if (start < oldest) start = oldest;  // caller fell behind the ring — resync

  static char line[2300];
  uint32_t sent = 0;
  for (uint32_t s = start; s <= maxseq && sent < limit; s++) {
    FrameRec &r = g_ring[(s - 1) % RING];
    if (r.seq != s) continue;  // overwritten since
    formatFrame(r, line, sizeof(line));
    server.sendContent(line);
    sent++;
  }
  server.sendContent("");  // terminate chunked response
}

static void handleSet() {
  if (server.hasArg("baud")) {
    uint32_t v = strtoul(server.arg("baud").c_str(), nullptr, 10);
    if (v >= 300 && v <= 1000000) g_baud = v;
  }
  if (server.hasArg("parity")) {
    char c = toupper(server.arg("parity").c_str()[0]);
    if (c == 'N' || c == 'E' || c == 'O') g_parity = c;
  }
  if (server.hasArg("gap")) {
    uint32_t v = strtoul(server.arg("gap").c_str(), nullptr, 10);
    if (v > 0) g_gap_us = v;
  }
  startBus();
  char st[300];
  statusLine(st, sizeof(st));
  server.send(200, "text/plain", String(st) + "\n");
}

static void handleMeasure() { server.send(200, "text/plain", measureBaud()); }

static void handleClear() {
  g_seq = 0;
  g_total_bytes = 0;
  g_total_frames = 0;
  g_rx_errors = 0;
  memset(g_ring, 0, sizeof(g_ring));
  server.send(200, "text/plain", "# cleared\n");
}

// Arm the 0x67 write test. ?ovr=reg:val,reg:val (val decimal or 0x..). e.g. ovr=11:0x5903
// (fan->high), or ovr=12:235,56:235 (main setpoint->23.5 in heat). Big-endian register values.
static void handleArm() {
  g_ovr_n = 0;
  if (server.hasArg("ovr")) {
    String s = server.arg("ovr");
    int start = 0;
    while (start < (int)s.length() && g_ovr_n < OVR_MAX) {
      int comma = s.indexOf(',', start);
      String pair = (comma < 0) ? s.substring(start) : s.substring(start, comma);
      int colon = pair.indexOf(':');
      if (colon > 0) {
        g_ovr_reg[g_ovr_n] = (uint16_t)strtoul(pair.substring(0, colon).c_str(), nullptr, 0);
        g_ovr_val[g_ovr_n] = (uint16_t)strtoul(pair.substring(colon + 1).c_str(), nullptr, 0);
        g_ovr_n++;
      }
      if (comma < 0) break;
      start = comma + 1;
    }
  }
  if (server.hasArg("turn")) {
    uint32_t t = strtoul(server.arg("turn").c_str(), nullptr, 0);
    if (t >= 100 && t <= 200000) g_turn_us = t;
  }
  if (server.hasArg("addr")) {
    uint8_t a = (uint8_t)strtoul(server.arg("addr").c_str(), nullptr, 0);
    if (a == 0x66 || a == 0x67 || a == 0x68) g_emul_addr = a;
  }
  g_pulse_n = 0;
  if (server.hasArg("pulse")) {       // ?pulse=reg:val (&pulsen=N, default 1): fire a one-shot
    String p = server.arg("pulse"); int c = p.indexOf(':');
    if (c > 0) {
      g_pulse_reg = (uint16_t)strtoul(p.substring(0, c).c_str(), nullptr, 0);
      g_pulse_val = (uint16_t)strtoul(p.substring(c + 1).c_str(), nullptr, 0);
      g_pulse_n = server.hasArg("pulsen") ? atoi(server.arg("pulsen").c_str()) : 1;
    }
  }
  g_write_armed = (g_ovr_n > 0);
  String b = "# write " + String(g_write_armed ? "ARMED" : "NOT armed (need ?ovr=reg:val)") + "\n";
  b += "# emulating addr 0x" + String(g_emul_addr, HEX) +
       (g_emul_addr == 0x66 ? "  (!! UNPLUG the real NEO first, or you'll collide)\n" : "\n");
  b += "# templates: page1 " + String(g_page1_valid ? "ready" : "MISSING") +
       ", page2 " + String(g_page2_valid ? "ready" : "MISSING") + "\n";
  b += "# turnaround: " + String(g_turn_us) + " us\n";
  for (int i = 0; i < g_ovr_n; i++)
    b += "#   reg " + String(g_ovr_reg[i]) + " <- 0x" + String(g_ovr_val[i], HEX) + " (persistent)\n";
  if (g_pulse_n > 0)
    b += "#   PULSE reg " + String(g_pulse_reg) + " <- 0x" + String(g_pulse_val, HEX) +
         " for next " + String(g_pulse_n) + " page-1 response(s)\n";
  b += "# will answer 67 03 00 02 00 7C polls; GET /disarm to stop.\n";
  server.send(200, "text/plain", b);
}

static void handleDisarm() {
  g_write_armed = false;
  server.send(200, "text/plain", "# write DISARMED (receive-only)\n");
}

// Definitive TX self-test: briefly act as master — wait for an idle gap, inject one read-poll
// to 0x66, then listen for its reply. If 0x66 answers, our transmit path physically works.
static void handleTxProbe() {
  uint32_t idleStart = micros();
  uint32_t t0 = millis();
  while (millis() - t0 < 2000) {           // wait for >=50 ms of bus silence
    if (Bus.available()) { Bus.read(); idleStart = micros(); }
    else if (micros() - idleStart > 50000) break;
  }
  uint8_t probe[8] = {0x66, 0x03, 0x00, 0x02, 0x00, 0x7C, 0, 0};
  uint16_t c = modbusCrc(probe, 6);
  probe[6] = (uint8_t)(c & 0xFF);
  probe[7] = (uint8_t)(c >> 8);
  while (Bus.available()) Bus.read();      // clear RX
  delayMicroseconds(500);
  Bus.write(probe, 8);
  Bus.flush();
  // listen ~60 ms for a 66 03 F8 response (the real master is idle, so it's ours)
  uint8_t last3[3] = {0, 0, 0};
  bool got = false;
  uint32_t deadline = millis() + 60;
  while (millis() < deadline) {
    while (Bus.available()) {
      uint8_t b = Bus.read();
      last3[0] = last3[1]; last3[1] = last3[2]; last3[2] = b;
      if (last3[0] == 0x66 && last3[1] == 0x03 && last3[2] == 0xF8) got = true;
    }
  }
  String r = got
    ? "# TX OK: 0x66 answered our injected poll -> transmit path works.\n"
    : "# NO reply: our poll didn't reach 0x66 -> TX not making it onto the bus.\n";
  server.send(200, "text/plain", r);
}

// HTTP-pull OTA: device downloads firmware from a URL it can reach (e.g. the little server on
// atlas) and self-flashes. Avoids ArduinoOTA's connect-back, which WSL's NAT can't satisfy.
static void handleUpdate() {
  if (!server.hasArg("url")) {
    server.send(400, "text/plain", "# need ?url=http://host:port/firmware.bin\n");
    return;
  }
  String url = server.arg("url");
  server.send(200, "text/plain", "# pulling firmware: " + url + "\n");
  Serial.printf("# OTA: pulling %s\n", url.c_str());
  g_capture = false;  // stop touching the bus while flashing
  WiFiClient client;
  httpUpdate.rebootOnUpdate(true);
  t_httpUpdate_return r = httpUpdate.update(client, url);
  Serial.printf("# OTA failed (%d): %s\n", (int)r, httpUpdate.getLastErrorString().c_str());
  g_capture = true;  // only reached on failure — success reboots into the new image
}

// ---- USB console (fallback) ----------------------------------------------
static void pumpConsole() {
  static char buf[32];
  static size_t len = 0;
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\r') continue;
    if (c == '\n') {
      buf[len] = '\0';
      char st[300];
      switch (buf[0]) {
        case 'm': Serial.print(measureBaud()); break;
        case 's': statusLine(st, sizeof(st)); Serial.println(st); break;
        default:  Serial.println("# use the HTTP API; 's' status, 'm' measure"); break;
      }
      len = 0;
    } else if (len < sizeof(buf) - 1) {
      buf[len++] = c;
    }
  }
}

static void connectWifi() {
  WiFi.mode(WIFI_STA);
  WiFi.setHostname(HOSTNAME);
  WiFi.setSleep(false);
  WiFi.begin(WIFI_SSID, WIFI_PASS);
  Serial.printf("# WiFi connecting to \"%s\"", WIFI_SSID);
  uint32_t t0 = millis();
  while (!WiFi.isConnected() && millis() - t0 < 15000) {
    delay(250);
    Serial.print('.');
  }
  Serial.println();
  if (WiFi.isConnected()) {
    Serial.printf("# WiFi up: http://%s/  (or http://%s.local/)\n",
                  WiFi.localIP().toString().c_str(), HOSTNAME);
    if (MDNS.begin(HOSTNAME)) MDNS.addService("http", "tcp", 80);
    ArduinoOTA.setHostname(HOSTNAME);
    ArduinoOTA.begin();  // wireless reflash from here on
  } else {
    Serial.println("# WiFi failed — capture still runs, HTTP unavailable. Check creds.");
  }
}

void setup() {
  Serial.begin(115200);

#ifdef RGB_BUILTIN
  pinMode(RGB_PWR, OUTPUT);
  digitalWrite(RGB_PWR, HIGH);
#endif

  startBus();

  uint32_t t0 = millis();
  while (!Serial && millis() - t0 < 1500) delay(10);
  Serial.println("\n# Actron RS485 sniffer (listen-only)");

  connectWifi();

  server.on("/", handleRoot);
  server.on("/stats", handleStats);
  server.on("/log", handleLog);
  server.on("/set", handleSet);
  server.on("/measure", handleMeasure);
  server.on("/clear", handleClear);
  server.on("/update", handleUpdate);
  server.on("/armwrite", handleArm);
  server.on("/disarm", handleDisarm);
  server.on("/txprobe", handleTxProbe);
  server.begin();

  char st[300];
  statusLine(st, sizeof(st));
  Serial.println(st);
}

void loop() {
  ArduinoOTA.handle();
  server.handleClient();
  pumpConsole();
  if (g_capture) pumpCapture();
}
