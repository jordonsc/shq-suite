// Actron NEO <-> indoor-unit RS485 tool for the Unexpected Maker TinyC6 (ESP32-C6).
//
// The bus is Modbus RTU 9600/8N1; the indoor board is master, wall controllers are slaves at
// 0x66 (the NEO) / 0x67 / 0x68. Two operating modes:
//
//   1. Tap mode (one UART, both ends of the bus tied together in parallel) — the original
//      sniffer. UART1 on GPIO16/17 captures Modbus traffic and can optionally emulate a
//      secondary controller at 0x67 to send command pulses (mode/fan/main setpoint writes).
//      See FINDINGS.md §7/§9. Default state is disarmed/receive-only.
//
//   2. MITM bridge mode (TWO UARTs, bus physically cut between board and NEO) — the new
//      path. UART1 (GPIO16/17) talks to the indoor board; UART0 (GPIO0/1) talks to the NEO.
//      Bytes received on one side are forwarded to the other, with optional inline register
//      substitution + CRC re-stamping on the NEO->board direction (where the per-zone
//      setpoints live — see FINDINGS §10). The 0x67 emulator is force-disarmed whenever the
//      bridge is enabled; the two are mutually exclusive (both would drive UART1 TX).
//
// HTTP (port 80): read-only = GET, mutating/bus-driving/OTA = POST (query args still parsed).
//   GET  /            help + status
//   GET  /stats       one-line status (seq_max, armed, addr, tmpl1/2, poll/tx, bridge mode)
//   GET  /log         recent frames (interleaved A/B); ?since=<seq>&n=<max>
//   GET  /measure     estimate baud from raw line pulse widths on UART1 (~5s)
//   POST /set         ?baud=&parity=N|E|O&gap=<us>   change capture settings (use gap=5000)
//   POST /clear       reset ring + counters
//   POST /armwrite    ?addr=&ovr=reg:val,...&pulse=reg:val&pulsen=N&turn=us   ARM 0x67 emulator
//   POST /disarm      back to receive-only (also disables bridge)
//   POST /txprobe     TX self-test on UART1 (inject a poll to 0x66, report if it answers)
//   POST /bridge      ?mode=off|passthru|inject     set MITM bridge mode (both directions)
//   POST /inject      ?rules=reg:val,reg:val,...    set injection rules (NEO->board direction)
//   POST /loopback    ?n=<bytes>   send a test pattern UART1 TX -> UART0 RX + UART0 TX -> UART1
//                                 RX, verify byte integrity (dual-UART bench test)
//   POST /update      ?url=<bin>   HTTP-pull OTA
//
// Wiring:
//   UART1 (board side):  module RXD/DI <- GPIO16, module TXD/RO -> GPIO17, + 3V3/GND
//   UART0 (NEO side):    module RXD/DI <- GPIO1,  module TXD/RO -> GPIO0,  + 3V3/GND
//     (RX/TX inverted vs. UART1 — the C6's GPIO matrix is symmetric; chosen for wiring convenience)
//   Auto-direction transceivers — no DE/RE pin (driver keys off UART TX activity).
//   See WIRING.md / CLAUDE.md for the RJ45 pass-through tap and the cut-bus MITM tap.

#include <Arduino.h>
#include <WiFi.h>
#include <WebServer.h>
#include <ESPmDNS.h>
#include <ArduinoOTA.h>
#include <HTTPUpdate.h>
#include <esp_app_desc.h>
#include <esp_ota_ops.h>

#include "bridge.h"
#include "state.h"
#include "wifi_prov.h"
#include "ws_api.h"

// WiFi credentials are provisioned at runtime (NVS) via the SoftAP captive portal — see
// wifi_prov.{h,cpp}. There are NO baked creds and no secrets.h: an image built without creds
// used to boot but never join WiFi, and OTA-flashing that onto the in-wall device knocked it off
// the network with no remote recovery (ledger shq-suite-0013). With provisioning, an
// unprovisioned / failed-to-connect device falls back to its own AP (actron-mitm-XXXX) for setup.

// Application identity — embedded in the native esp_app_desc (app_desc.cpp), surfaced in
// /stats + /stats.json, and enforced by the OTA guard (handleUpdate) so the somfy-sdn firmware
// (same board, same /update endpoint) can't be flashed onto this in-wall Actron controller.
static const char *APP_ID = "actron-mitm";

// ---- pins -----------------------------------------------------------------
// Endpoint A = indoor board side, UART1 (the existing transceiver).
static const uint8_t PIN_A_RX = 17;  // module TXD/RO -> board "RX"
static const uint8_t PIN_A_TX = 16;  // module RXD/DI <- board "TX"
// Endpoint B = NEO side, UART0 (new — added for the MITM cut-bus bridge).
// Note: GPIO0 = RX, GPIO1 = TX — the opposite of the A-endpoint's pin order, chosen post
// bench-test to avoid a re-solder on module B (its DI was wired to GPIO1 and RO to GPIO0).
// The C6's GPIO matrix lets either UART target any pin, so this is purely a software swap.
static const uint8_t PIN_B_RX = 0;
static const uint8_t PIN_B_TX = 1;

WebServer server(80);

// ---- frame ring buffer ----------------------------------------------------
static const size_t FRAME_MAX = 512;  // capture whole messages (~253 B) — no 128 B splitting
static const size_t RING = 128;
struct FrameRec {
  uint32_t seq;     // monotonic, 1-based; 0 = empty slot
  uint32_t t_ms;
  uint32_t gap_us;
  uint16_t len;
  uint8_t  source;  // 0 = A (board), 1 = B (NEO)
  uint8_t  data[FRAME_MAX];
};
static FrameRec g_ring[RING];
static uint32_t g_seq = 0;

// ---- per-endpoint state ---------------------------------------------------
struct BusEndpoint {
  const char* name;
  HardwareSerial& uart;
  uint8_t pin_rx, pin_tx;
  uint8_t source_tag;

  // Frame assembly
  uint8_t  frame[FRAME_MAX];
  size_t   flen;
  uint32_t first_byte_us;
  uint32_t last_byte_us;
  uint32_t prev_frame_end_us;

  // Counters
  uint64_t total_bytes;
  uint32_t total_frames;
  uint32_t modified_frames;
  volatile uint32_t rx_errors;

  // Bridge: substitution applied to bytes RECEIVED on this endpoint, going TO the peer.
  bridge::StreamingBridge bridge;

  BusEndpoint(const char* n, HardwareSerial& u, uint8_t rx, uint8_t tx, uint8_t tag)
    : name(n), uart(u), pin_rx(rx), pin_tx(tx), source_tag(tag),
      flen(0), first_byte_us(0), last_byte_us(0), prev_frame_end_us(0),
      total_bytes(0), total_frames(0), modified_frames(0), rx_errors(0) {}
};

static BusEndpoint g_a("A", Serial1, PIN_A_RX, PIN_A_TX, 0);
static BusEndpoint g_b("B", Serial0, PIN_B_RX, PIN_B_TX, 1);

// ---- live-configurable capture settings ----------------------------------
static uint32_t g_baud = 9600;
static char g_parity = 'N';        // N | E | O
static uint32_t g_gap_us = 5000;   // idle gap that ends a frame (>= Modbus t3.5 @9600 ~= 3.65ms)
static bool g_capture = true;

// ---- Per-frame "scramble" of one register on B->A (NEO->board) ----------
// Tests the hypothesis that the board's commit gate for page-2 zone setpoints is "the NEO's
// reg 248 differs from the last value I saw" rather than a deterministic hash. When enabled,
// on every NEW B page-2 frame we increment a nonce and upsert a rule for g_scramble_reg into
// the bridge so the rewritten frame carries a fresh value at that register. The hop step
// (0x9E37 ≈ golden-ratio fractional × 2^16) spreads the nonce evenly across 16 bits.
static uint16_t g_scramble_reg = 0;             // 0 = disabled
static uint16_t g_scramble_nonce = 0xACE0;      // seed

// ---- MITM bridge mode (overall) ------------------------------------------
// OFF: tap-style capture only (the original behaviour; the 0x67 emulator can run).
//      DANGER if the bus is physically cut — the NEO and indoor board are then isolated.
// PASSTHRU: both endpoints forward received bytes to the peer's TX, no rewrites. Safe
//      boot state for tap-mode operators not running the WS controller API.
// INJECT (DEFAULT): same as PASSTHRU plus the rules apply to the NEO->board direction.
//      Boot default for the cut-bus MITM deployment so the WS API can issue writes
//      without an HTTP poke first. With zero rules INJECT adds ~166 ns/byte on top of
//      PASSTHRU vs. a 1.04 ms byte window at 9600 baud — invisible in practice.
// RESPOND: NEO is blocked entirely; firmware impersonates 0x66 to the board from loaded
//      replay templates. Operator-driven, exits on next /bridge?mode=.
static bridge::StreamingBridge::Mode g_bridge_mode = bridge::StreamingBridge::INJECT;

// State decoded from the NEO frames the bridge has seen — published to WS clients on
// change. Kept on the main loop's side and copied into ws_api via publishStateIfChanged.
static state::ControllerState g_ctrl_state;

// ---- 0x67 controller emulation (tap mode only) ---------------------------
// When ARMED, answer the indoor board's `addr 03 00 02 00 7C` page-1 poll with a forged but
// CRC-valid response built from the cached 0x66 template, with overrides + optional one-shot
// command pulse. Mutually exclusive with bridge mode != OFF (both would drive UART1 TX).
static bool g_write_armed = false;
static uint8_t g_emul_addr = 0x67;
static uint8_t g_page1[248];     // regs 2..125   from the last good 66 03 F8 response
static uint8_t g_page2[244];     // regs 126..247 from the last good 66 03 F4 response
static bool g_page1_valid = false;
static bool g_page2_valid = false;
static const int OVR_MAX = 8;
static uint16_t g_ovr_reg[OVR_MAX];
static uint16_t g_ovr_val[OVR_MAX];
static int g_ovr_n = 0;
static uint32_t g_poll67 = 0;
static uint32_t g_tx67 = 0;
static uint32_t g_turn_us = 5000;
static uint16_t g_pulse_reg = 0;
static uint16_t g_pulse_val = 0;
static int g_pulse_n = 0;

// Pure-replay cache (separate from the 0x67 emulator templates above — these include the 2
// trailing CRC bytes so the bridge can byte-for-byte forward a recorded NEO response with its
// original CRC). Updated whenever a CRC-valid 0x66 response is captured.
static uint8_t g_replay_p1[bridge::StreamingBridge::kReplayPage1Bytes];  // 248 data + 2 CRC
static uint8_t g_replay_p2[bridge::StreamingBridge::kReplayPage2Bytes];  // 244 data + 2 CRC
static bool g_replay_p1_valid = false;
static bool g_replay_p2_valid = false;

static uint16_t modbusCrc(const uint8_t *d, size_t n) { return bridge::crc16(d, n); }

// Setter callback for ws_api.h — flips both endpoints' bridge instances in lock-step with
// the global mode variable. Also force-disarms the 0x67 emulator (mutex on UART1 TX).
static void setBridgeMode(bridge::StreamingBridge::Mode m) {
  g_bridge_mode = m;
  g_a.bridge.setMode(m);
  g_b.bridge.setMode(m);
  if (m != bridge::StreamingBridge::OFF) g_write_armed = false;
}

// Cache the latest complete, CRC-valid 0x66 response as our forged-response template (used by
// the 0x67 emulator). Page 1 = 66 03 F8 (253 B); page 2 = 66 03 F4 (249 B).
static void maybeCacheTemplates(const uint8_t *f, size_t len) {
  if (f[0] != 0x66 || f[1] != 0x03) return;
  if (len == 253 && f[2] == 0xF8 && modbusCrc(f, 251) == (uint16_t)(f[251] | (f[252] << 8))) {
    memcpy(g_page1, f + 3, 248); g_page1_valid = true;
    memcpy(g_replay_p1, f + 3, 250); g_replay_p1_valid = true;   // data + CRC for pure replay
  } else if (len == 249 && f[2] == 0xF4 && modbusCrc(f, 247) == (uint16_t)(f[247] | (f[248] << 8))) {
    memcpy(g_page2, f + 3, 244); g_page2_valid = true;
    memcpy(g_replay_p2, f + 3, 246); g_replay_p2_valid = true;
  }
}

static uint32_t parityConfig() {
  switch (g_parity) {
    case 'E': return SERIAL_8E1;
    case 'O': return SERIAL_8O1;
    default:  return SERIAL_8N1;
  }
}

static void startBusEndpoint(BusEndpoint& ep) {
  ep.uart.end();
  ep.uart.setRxBufferSize(4096);
  ep.uart.begin(g_baud, parityConfig(), ep.pin_rx, ep.pin_tx);
  ep.uart.onReceiveError([&ep](hardwareSerial_error_t e) {
    if (e == UART_FRAME_ERROR || e == UART_PARITY_ERROR) ep.rx_errors++;
  });
}

static void startBuses() {
  startBusEndpoint(g_a);
  startBusEndpoint(g_b);
}

static void ledActivity() {
#ifdef RGB_BUILTIN
  static bool on = false;
  on = !on;
  rgbLedWrite(RGB_BUILTIN, 0, on ? 6 : 0, 0);
#endif
}

static size_t formatFrame(const FrameRec &f, char *out, size_t cap) {
  size_t n = snprintf(out, cap, "%c %u %.6f +%uus %u:",
                      f.source ? 'B' : 'A', f.seq, f.t_ms / 1000.0, f.gap_us, f.len);
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

static void flushFrame(BusEndpoint& ep, bool was_modified) {
  if (ep.flen == 0) return;
  uint32_t s = ++g_seq;
  FrameRec &r = g_ring[(s - 1) % RING];
  r.seq = s;
  r.t_ms = ep.first_byte_us / 1000;
  r.gap_us = ep.prev_frame_end_us ? (ep.first_byte_us - ep.prev_frame_end_us) : 0;
  r.len = (ep.flen > FRAME_MAX) ? FRAME_MAX : ep.flen;
  r.source = ep.source_tag;
  memcpy(r.data, ep.frame, r.len);
  if (ep.source_tag == 0) maybeCacheTemplates(r.data, r.len);

  // State decode runs on the B (NEO) side — those are the ORIGINAL NEO bytes pre-bridge-
  // modification, so our own pending INJECT writes don't get fed back as "the board has
  // committed". The NEO syncs to the board's broadcast each polling cycle, so its responses
  // here accurately reflect committed state with ~1-cycle lag.
  if (ep.source_tag == 1) {
    if (state::feedNeoFrame(g_ctrl_state, r.data, r.len)) {
      ws_api::publishStateIfChanged(g_ctrl_state);
    }
  }

  ep.total_frames++;
  if (was_modified) ep.modified_frames++;
  ep.prev_frame_end_us = ep.last_byte_us;

  static char line[2300];
  formatFrame(r, line, sizeof(line));
  Serial.print(line);

  ep.flen = 0;
  ledActivity();
}

// 0x67 emulator response (tap mode). page2=false -> regs 2..125 (66 03 F8); true -> 126..247.
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
  if (!page2 && g_pulse_n > 0 && g_pulse_reg >= firstReg && g_pulse_reg <= lastReg) {
    size_t off = 3 + (size_t)(g_pulse_reg - firstReg) * 2;
    resp[off]     = (uint8_t)(g_pulse_val >> 8);
    resp[off + 1] = (uint8_t)(g_pulse_val & 0xFF);
    g_pulse_n--;
  }
  size_t total = 3 + ndata;
  uint16_t c = modbusCrc(resp, total);
  resp[total]     = (uint8_t)(c & 0xFF);
  resp[total + 1] = (uint8_t)(c >> 8);
  delayMicroseconds(g_turn_us);
  g_a.uart.write(resp, total + 2);
  g_a.uart.flush();
  g_tx67++;
}

static void emitFromTemplate(uint8_t bytecount, const uint8_t* tmpl, size_t tmpl_len);

// Pump RX from one endpoint: capture frames, forward bytes to peer (when bridge != OFF),
// fire 0x67 emulator on a poll match (only when bridge == OFF on UART1).
static void pumpEndpoint(BusEndpoint& ep, BusEndpoint& peer) {
  const bool bridging = (g_bridge_mode != bridge::StreamingBridge::OFF);
  const bool respond_mode = (g_bridge_mode == bridge::StreamingBridge::RESPOND);

  uint32_t now = micros();
  if (ep.flen > 0 && (now - ep.last_byte_us) > g_gap_us) {
    bool was_mod = bridging && ep.bridge.modified();
    flushFrame(ep, was_mod);
    if (bridging) ep.bridge.onGap();
  }

  while (ep.uart.available()) {
    int b = ep.uart.read();
    if (b < 0) break;
    now = micros();
    if (ep.flen > 0 && (now - ep.last_byte_us) > g_gap_us) {
      bool was_mod = bridging && ep.bridge.modified();
      flushFrame(ep, was_mod);
      // Avoid resetting the bridge mid-frame on a false-positive idle (a short "gap" can be
      // caused by our own main-loop latency — HTTP handling, TX-FIFO backpressure — not a real
      // wire gap). If we reset mid-frame the rest of the response gets treated as a new
      // unrecognised frame, losing the CRC re-stamp and putting a broken frame on the wire.
      // Strategy: skip the bridge reset if it's currently inside a recognised frame; but ALSO
      // force a reset on any truly long idle (>30 ms) so we don't permanently desync on a real
      // bus error. The bridge auto-resets internally on clean frame completion either way.
      if (bridging && (!ep.bridge.isMidFrame() || (now - ep.last_byte_us) > 30000)) {
        ep.bridge.onGap();
      }
    }
    if (ep.flen == 0) {
      ep.first_byte_us = now;
      // New frame starting on the NEO side — refresh the scramble nonce (if armed) so the
      // rewritten frame carries a fresh value at g_scramble_reg.
      if (bridging && &ep == &g_b && g_scramble_reg != 0) {
        g_scramble_nonce = (uint16_t)(g_scramble_nonce + 0x9E37);
        g_b.bridge.setOrAddRule(g_scramble_reg, g_scramble_nonce);
      }
    }
    if (ep.flen < FRAME_MAX) ep.frame[ep.flen++] = (uint8_t)b;
    ep.last_byte_us = now;
    ep.total_bytes++;

    // MITM forward: rewrite (if INJECT) and emit to the peer's TX. Skipped in RESPOND mode —
    // the NEO is fully isolated and the firmware impersonates 0x66 to the board.
    if (bridging && !respond_mode) {
      uint8_t out = ep.bridge.processByte((uint8_t)b);
      peer.uart.write(out);
    }

    // 0x67 emulator: only when bridging is OFF, only on the board-side UART (A).
    if (!bridging && &ep == &g_a && g_write_armed && ep.flen == 8 &&
        ep.frame[0] == g_emul_addr && ep.frame[1] == 0x03 &&
        ep.frame[2] == 0x00 && ep.frame[4] == 0x00 &&
        modbusCrc(ep.frame, 6) == (uint16_t)(ep.frame[6] | (ep.frame[7] << 8))) {
      bool p1 = (ep.frame[3] == 0x02 && ep.frame[5] == 0x7C);
      bool p2 = (ep.frame[3] == 0x7E && ep.frame[5] == 0x7A);
      if (p1 || p2) {
        g_poll67++;
        flushFrame(ep, false);
        emitResponse(p2);
        continue;
      }
    }

    // RESPOND mode: impersonate 0x66 to the board using the loaded replay templates.
    // Triggered when the board's page-1 (00 02 / 00 7C) or page-2 (00 7E / 00 7A) read poll
    // arrives on UART1 and CRC validates. Templates already contain the original NEO CRC, so
    // we forward bytes 3..end verbatim with a fresh 3-byte header prepended.
    if (respond_mode && &ep == &g_a && ep.flen == 8 &&
        ep.frame[0] == 0x66 && ep.frame[1] == 0x03 &&
        ep.frame[2] == 0x00 && ep.frame[4] == 0x00 &&
        modbusCrc(ep.frame, 6) == (uint16_t)(ep.frame[6] | (ep.frame[7] << 8))) {
      bool p1 = (ep.frame[3] == 0x02 && ep.frame[5] == 0x7C);
      bool p2 = (ep.frame[3] == 0x7E && ep.frame[5] == 0x7A);
      flushFrame(ep, false);
      if (p1 && g_replay_p1_valid) {
        emitFromTemplate(0xF8, g_replay_p1, bridge::StreamingBridge::kReplayPage1Bytes);
      } else if (p2 && g_replay_p2_valid) {
        emitFromTemplate(0xF4, g_replay_p2, bridge::StreamingBridge::kReplayPage2Bytes);
      }
      if (p1 || p2) continue;
    }

    if (ep.flen == FRAME_MAX) {
      bool was_mod = bridging && ep.bridge.modified();
      flushFrame(ep, was_mod);
      // Avoid resetting the bridge mid-frame on a false-positive idle (a short "gap" can be
      // caused by our own main-loop latency — HTTP handling, TX-FIFO backpressure — not a real
      // wire gap). If we reset mid-frame the rest of the response gets treated as a new
      // unrecognised frame, losing the CRC re-stamp and putting a broken frame on the wire.
      // Strategy: skip the bridge reset if it's currently inside a recognised frame; but ALSO
      // force a reset on any truly long idle (>30 ms) so we don't permanently desync on a real
      // bus error. The bridge auto-resets internally on clean frame completion either way.
      if (bridging && (!ep.bridge.isMidFrame() || (now - ep.last_byte_us) > 30000)) {
        ep.bridge.onGap();
      }
    }
  }
}

static void pumpCapture() {
  pumpEndpoint(g_a, g_b);
  pumpEndpoint(g_b, g_a);
}

// Dedicated FreeRTOS task for byte forwarding. Runs at a priority above the Arduino main loop
// (which does HTTP / OTA / mDNS / console) so a slow HTTP request can't stall the bridge mid-
// frame and insert a >t3.5 gap on the destination wire — which the indoor board would treat as
// premature frame-end, rejecting the NEO's response on CRC. The pump busy-loops while either
// UART has data pending (so a Modbus burst gets forwarded without preemption), and only yields
// during the genuine idle window between bursts (Modbus master pacing leaves >6 ms here, which
// is plenty of room for HTTP / OTA work).
static TaskHandle_t g_bridge_task = nullptr;
static void bridgeTask(void* /*arg*/) {
  for (;;) {
    if (g_capture) {
      pumpCapture();
      if (!g_a.uart.available() && !g_b.uart.available()) {
        vTaskDelay(1);  // bus idle: let lower-priority main loop run
      }
    } else {
      // Capture disabled (e.g. OTA, loopback test). pumpCapture is the only thing that
      // drains UART RX buffers, so without it `available()` is sticky-true once any byte
      // arrives — the previous "yield only on idle" branch would never fire and starve
      // the priority-1 main loop. Always yield when capture is off.
      vTaskDelay(1);
    }
  }
}

static const char* bridgeModeStr(bridge::StreamingBridge::Mode m) {
  switch (m) {
    case bridge::StreamingBridge::OFF: return "off";
    case bridge::StreamingBridge::PASSTHRU: return "passthru";
    case bridge::StreamingBridge::INJECT: return "inject";
    case bridge::StreamingBridge::RESPOND: return "respond";
  }
  return "?";
}

// Forge a 0x66 func-03 response from a replay template (data + trailing CRC, byte-for-byte).
// Used only in RESPOND mode — the firmware impersonates 0x66 to the board while the NEO is
// fully isolated (no forwarding either direction). The template's original CRC is valid
// because the prepended header bytes (0x66 0x03 <bytecount>) are identical to whatever was
// on the wire when the capture was taken.
static uint32_t g_respond_tx = 0;
static void emitFromTemplate(uint8_t bytecount, const uint8_t* tmpl, size_t tmpl_len) {
  static uint8_t buf[256];
  buf[0] = 0x66;
  buf[1] = 0x03;
  buf[2] = bytecount;
  memcpy(buf + 3, tmpl, tmpl_len);
  delayMicroseconds(g_turn_us);
  g_a.uart.write(buf, 3 + tmpl_len);
  g_a.uart.flush();
  g_respond_tx++;
}

static size_t statusLine(char *out, size_t cap) {
  return snprintf(out, cap,
    "# app=actron-mitm seq_max=%u baud=%u parity=8%c1 gap=%uus capture=%s "
    "A.bytes=%llu A.frames=%u A.mod=%u A.rxerr=%u "
    "B.bytes=%llu B.frames=%u B.mod=%u B.rxerr=%u "
    "rssi=%d ip=%s "
    "armed=%d addr=0x%02X tmpl1=%d tmpl2=%d poll=%u tx=%u "
    "bridge=%s rules=%u pulse_rules=%u pulse_remaining=%u "
    "scramble=%u nonce=0x%04X fw=\"%s\"",
    g_seq, g_baud, g_parity, g_gap_us, g_capture ? "on" : "off",
    (unsigned long long)g_a.total_bytes, g_a.total_frames, g_a.modified_frames, g_a.rx_errors,
    (unsigned long long)g_b.total_bytes, g_b.total_frames, g_b.modified_frames, g_b.rx_errors,
    WiFi.isConnected() ? WiFi.RSSI() : 0,
    WiFi.localIP().toString().c_str(),
    g_write_armed ? 1 : 0, g_emul_addr, g_page1_valid ? 1 : 0, g_page2_valid ? 1 : 0,
    g_poll67, g_tx67,
    bridgeModeStr(g_bridge_mode), (unsigned)g_b.bridge.ruleCount(),
    (unsigned)g_b.bridge.pulseRuleCount(), (unsigned)g_b.bridge.pulseRemaining(),
    g_scramble_reg, g_scramble_nonce,
    __DATE__ " " __TIME__);
}

// ---- pulse-width baud estimator (UART1 / board side only) -----------------
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
  g_a.uart.end();
  pinMode(g_a.pin_rx, INPUT);
  g_edge_n = 0;
  attachInterrupt(digitalPinToInterrupt(g_a.pin_rx), edgeISR, CHANGE);
  uint32_t deadline = millis() + 5000;
  while (millis() < deadline && g_edge_n < EDGE_CAP) delay(1);
  detachInterrupt(digitalPinToInterrupt(g_a.pin_rx));

  size_t n = g_edge_n;
  String out;
  if (n < 8) {
    out = "# only " + String((unsigned)n) +
          " edges — idle/no traffic on UART1. Trigger a change and retry.\n";
    startBuses();
    return out;
  }

  uint32_t mins[5] = {UINT32_MAX, UINT32_MAX, UINT32_MAX, UINT32_MAX, UINT32_MAX};
  for (size_t i = 1; i < n; i++) {
    uint32_t d = g_edges[i] - g_edges[i - 1];
    if (d < 3) continue;
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
  startBuses();
  return out;
}

// ---- HTTP handlers --------------------------------------------------------
static void handleRoot() {
  char st[600];
  statusLine(st, sizeof(st));
  String b = "Actron RS485 sniffer + MITM bridge\n";
  b += String(st) + "\n\n";
  b += "GET  /stats              status line\n";
  b += "GET  /log?since=<seq>&n=<max>  frames (interleaved A/B, incremental)\n";
  b += "GET  /measure            estimate baud from UART1 line pulses (~5s)\n";
  b += "POST /set?baud=&parity=N|E|O&gap=<us>\n";
  b += "POST /clear              reset ring + counters\n";
  b += "POST /armwrite?ovr=reg:val,...  ARM 0x67 emulator (tap mode, requires bridge=off)\n";
  b += "POST /disarm             stop mutations: 0x67 emulator off, INJECT->PASSTHRU\n";
  b += "POST /txprobe            TX self-test on UART1 (requires bridge=off)\n";
  b += "POST /bridge?mode=off|passthru|inject|respond  MITM bridge mode (default: passthru)\n";
  b += "                                        respond = block NEO, impersonate 0x66 to board from loaded templates\n";
  b += "POST /inject?rules=reg:val,...         NEO->board substitution rules (persistent)\n";
  b += "POST /pulse?rules=reg:val,...&n=<N>    transient rules for next N recognised B frames\n";
  b += "POST /snapshot                         freeze the last 0x66 page1+page2 (with CRC) as replay templates\n";
  b += "POST /replay?on=0|1                    byte-for-byte playback of snapshot (no CRC recompute)\n";
  b += "POST /loadtemplate?page=1|2&hex=<bytes> load saved payload into replay template (data + CRC, no header)\n";
  b += "POST /scramble?reg=<n>&on=0|1          per-frame nonce rewrite on reg (commit-gate test)\n";
  b += "POST /loopback?n=<bytes>               dual-UART bench: 256/1024-byte pattern + verify\n";
  b += "POST /uartcheck                        1-byte per-direction probe (diagnostic)\n";
  b += "POST /blink                            6x 50-byte bursts each UART, watch the LEDs\n";
  b += "POST /update?url=<bin>                 HTTP-pull OTA\n";
  b += "\n# mutating endpoints are POST; curl -X POST (query args still work)\n";
  server.send(200, "text/plain", b);
}

static void handleStats() {
  char st[600];
  statusLine(st, sizeof(st));
  server.send(200, "text/plain", String(st) + "\n");
}

// Machine-readable identity + status. Parallels somfy-sdn's /stats.json so one uniform endpoint
// positively identifies any TinyC6 board on the LAN (app/mac), rather than inferring from which
// text /stats fields happen to be present.
static void handleStatsJson() {
  char buf[320];
  snprintf(buf, sizeof(buf),
    "{\"app\":\"%s\",\"model\":\"TinyC6\",\"fw\":\"%s\",\"hostname\":\"%s\","
    "\"ip\":\"%s\",\"mac\":\"%s\",\"rssi\":%d,\"bridge\":\"%s\"}",
    APP_ID, __DATE__ " " __TIME__, wifi_prov::hostname(),
    WiFi.localIP().toString().c_str(), WiFi.macAddress().c_str(),
    WiFi.isConnected() ? WiFi.RSSI() : 0, bridgeModeStr(g_bridge_mode));
  server.send(200, "application/json", buf);
}

static void handleLog() {
  uint32_t since = server.hasArg("since")
                       ? strtoul(server.arg("since").c_str(), nullptr, 10) : 0;
  uint32_t limit = server.hasArg("n")
                       ? strtoul(server.arg("n").c_str(), nullptr, 10) : 1000;

  server.setContentLength(CONTENT_LENGTH_UNKNOWN);
  server.send(200, "text/plain", "");
  char st[600];
  statusLine(st, sizeof(st));
  server.sendContent(String(st) + "\n");

  uint32_t maxseq = g_seq;
  uint32_t oldest = (maxseq > RING) ? (maxseq - RING + 1) : 1;
  uint32_t start = since + 1;
  if (start < oldest) start = oldest;

  static char line[2300];
  uint32_t sent = 0;
  for (uint32_t s = start; s <= maxseq && sent < limit; s++) {
    FrameRec &r = g_ring[(s - 1) % RING];
    if (r.seq != s) continue;
    formatFrame(r, line, sizeof(line));
    server.sendContent(line);
    sent++;
  }
  server.sendContent("");
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
  startBuses();
  char st[600];
  statusLine(st, sizeof(st));
  server.send(200, "text/plain", String(st) + "\n");
}

static void handleMeasure() { server.send(200, "text/plain", measureBaud()); }

static void handleClear() {
  g_seq = 0;
  g_a.total_bytes = g_a.total_frames = g_a.modified_frames = g_a.rx_errors = 0;
  g_b.total_bytes = g_b.total_frames = g_b.modified_frames = g_b.rx_errors = 0;
  memset(g_ring, 0, sizeof(g_ring));
  server.send(200, "text/plain", "# cleared\n");
}

static void handleArm() {
  if (g_bridge_mode != bridge::StreamingBridge::OFF) {
    server.send(409, "text/plain",
      "# REJECTED: bridge is active (mode != off). /armwrite is mutually exclusive with the "
      "MITM bridge — both would drive UART1 TX. Run /bridge?mode=off first.\n");
    return;
  }
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
  if (server.hasArg("pulse")) {
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
  b += "# will answer polls; GET /disarm to stop.\n";
  server.send(200, "text/plain", b);
}

// /disarm — stop all *mutation*: 0x67 emulator off, INJECT bridge drops to PASSTHRU.
// The relay itself (PASSTHRU) stays up so a cut bus keeps working. OFF stays OFF (tap mode).
static void handleDisarm() {
  g_write_armed = false;
  if (g_bridge_mode == bridge::StreamingBridge::INJECT) {
    setBridgeMode(bridge::StreamingBridge::PASSTHRU);
  }
  String r = "# DISARMED: 0x67 emulator off, bridge mode -> ";
  r += bridgeModeStr(g_bridge_mode);
  r += "\n";
  server.send(200, "text/plain", r);
}

static void handleTxProbe() {
  if (g_bridge_mode != bridge::StreamingBridge::OFF) {
    server.send(409, "text/plain", "# REJECTED: bridge active. Run /bridge?mode=off first.\n");
    return;
  }
  uint32_t idleStart = micros();
  uint32_t t0 = millis();
  while (millis() - t0 < 2000) {
    if (g_a.uart.available()) { g_a.uart.read(); idleStart = micros(); }
    else if (micros() - idleStart > 50000) break;
  }
  uint8_t probe[8] = {0x66, 0x03, 0x00, 0x02, 0x00, 0x7C, 0, 0};
  uint16_t c = modbusCrc(probe, 6);
  probe[6] = (uint8_t)(c & 0xFF);
  probe[7] = (uint8_t)(c >> 8);
  while (g_a.uart.available()) g_a.uart.read();
  delayMicroseconds(500);
  g_a.uart.write(probe, 8);
  g_a.uart.flush();
  uint8_t last3[3] = {0, 0, 0};
  bool got = false;
  uint32_t deadline = millis() + 60;
  while (millis() < deadline) {
    while (g_a.uart.available()) {
      uint8_t b = g_a.uart.read();
      last3[0] = last3[1]; last3[1] = last3[2]; last3[2] = b;
      if (last3[0] == 0x66 && last3[1] == 0x03 && last3[2] == 0xF8) got = true;
    }
  }
  String r = got
    ? "# TX OK: 0x66 answered our injected poll -> transmit path works.\n"
    : "# NO reply: our poll didn't reach 0x66 -> TX not making it onto the bus.\n";
  server.send(200, "text/plain", r);
}

// /bridge?mode=off|passthru|inject — set MITM bridge mode for both endpoints.
// Forces the 0x67 emulator OFF when switching to a non-OFF mode (mutex on UART1 TX).
static void handleBridge() {
  if (!server.hasArg("mode")) {
    server.send(400, "text/plain", "# need ?mode=off|passthru|inject\n");
    return;
  }
  String m = server.arg("mode");
  m.toLowerCase();
  bridge::StreamingBridge::Mode new_mode;
  if      (m == "off")      new_mode = bridge::StreamingBridge::OFF;
  else if (m == "passthru") new_mode = bridge::StreamingBridge::PASSTHRU;
  else if (m == "inject")   new_mode = bridge::StreamingBridge::INJECT;
  else if (m == "respond")  new_mode = bridge::StreamingBridge::RESPOND;
  else { server.send(400, "text/plain", "# bad mode\n"); return; }

  setBridgeMode(new_mode);  // also handles 0x67 mutex

  String b = "# bridge mode -> " + String(bridgeModeStr(new_mode)) + "\n";
  b += "# rules (B->A direction): " + String((unsigned)g_b.bridge.ruleCount()) + "\n";
  if (new_mode != bridge::StreamingBridge::OFF)
    b += "# 0x67 emulator force-disarmed (mutually exclusive with bridge).\n";
  server.send(200, "text/plain", b);
}

// /inject?rules=reg:val,reg:val,... — set substitution rules for the NEO->board direction.
// Endpoint B is the NEO side; its bridge instance is the one that rewrites NEO -> board.
static void handleInject() {
  bridge::Rule rules[bridge::StreamingBridge::MAX_RULES];
  size_t n = 0;
  if (server.hasArg("rules")) {
    String s = server.arg("rules");
    int start = 0;
    while (start < (int)s.length() && n < bridge::StreamingBridge::MAX_RULES) {
      int comma = s.indexOf(',', start);
      String pair = (comma < 0) ? s.substring(start) : s.substring(start, comma);
      int colon = pair.indexOf(':');
      if (colon > 0) {
        rules[n].reg   = (uint16_t)strtoul(pair.substring(0, colon).c_str(), nullptr, 0);
        rules[n].value = (uint16_t)strtoul(pair.substring(colon + 1).c_str(), nullptr, 0);
        n++;
      }
      if (comma < 0) break;
      start = comma + 1;
    }
  }
  g_b.bridge.setRules(rules, n);   // bytes received on B (NEO) -> rewrite -> peer A (board)
  String b = "# " + String((unsigned)n) + " rule(s) set on B->A (NEO->board) direction\n";
  for (size_t i = 0; i < n; i++)
    b += "#   reg " + String(rules[i].reg) + " <- " + String(rules[i].value) +
         " (0x" + String(rules[i].value, HEX) + ")\n";
  if (g_bridge_mode != bridge::StreamingBridge::INJECT)
    b += "# NOTE: bridge mode is currently " + String(bridgeModeStr(g_bridge_mode)) +
         "; rules only take effect once you GET /bridge?mode=inject\n";
  server.send(200, "text/plain", b);
}

// /loadtemplate?page=1|2&hex=<bytes> — load a specific byte sequence (data + 2-byte CRC, no
// header) into the replay template buffer. Accepts hex with or without whitespace. Used to
// inject a *saved* NEO response (from disk) into the firmware so RESPOND mode can replay it.
// page=1 expects 250 bytes (248 data + 2 CRC); page=2 expects 246 bytes (244 data + 2 CRC).
static void handleLoadTemplate() {
  if (!server.hasArg("page") || !server.hasArg("hex")) {
    server.send(400, "text/plain", "# need ?page=1|2&hex=<bytes>\n");
    return;
  }
  int page = server.arg("page").toInt();
  if (page != 1 && page != 2) {
    server.send(400, "text/plain", "# page must be 1 or 2\n");
    return;
  }
  size_t expected = (page == 1) ? bridge::StreamingBridge::kReplayPage1Bytes
                                : bridge::StreamingBridge::kReplayPage2Bytes;
  String hex = server.arg("hex");
  auto hv = [](char c) -> int {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    return -1;
  };
  uint8_t buf[256];
  size_t n = 0;
  size_t i = 0;
  while (i < (size_t)hex.length() && n < expected) {
    while (i < (size_t)hex.length() &&
           (hex[i] == ' ' || hex[i] == '\n' || hex[i] == '\r' || hex[i] == '\t')) i++;
    if (i + 1 >= (size_t)hex.length()) break;
    int h1 = hv(hex[i]); int h2 = hv(hex[i + 1]);
    if (h1 < 0 || h2 < 0) {
      server.send(400, "text/plain",
        String("# bad hex char at position ") + String((int)i) + "\n");
      return;
    }
    buf[n++] = (uint8_t)((h1 << 4) | h2);
    i += 2;
  }
  if (n != expected) {
    server.send(400, "text/plain",
      String("# need exactly ") + String((unsigned)expected) +
      " bytes, parsed " + String((unsigned)n) + "\n");
    return;
  }
  if (page == 1) {
    memcpy(g_replay_p1, buf, bridge::StreamingBridge::kReplayPage1Bytes);
    g_replay_p1_valid = true;
    g_b.bridge.setReplayPage1Data(g_replay_p1);
  } else {
    memcpy(g_replay_p2, buf, bridge::StreamingBridge::kReplayPage2Bytes);
    g_replay_p2_valid = true;
    g_b.bridge.setReplayPage2Data(g_replay_p2);
  }
  server.send(200, "text/plain",
    String("# loaded ") + String((unsigned)n) + " bytes into replay page " + String(page) + "\n");
}

// /snapshot — freeze the last CRC-valid page-1 and page-2 NEO responses into the bridge's
// replay buffers. The cache (g_replay_p1, g_replay_p2) is continually updated from live
// captures; snapshot just hands the current contents to the bridge as its replay templates.
static void handleSnapshot() {
  String b = "# snapshot taken: ";
  if (g_replay_p1_valid) { g_b.bridge.setReplayPage1Data(g_replay_p1); b += "page1=loaded "; }
  else                   { b += "page1=MISSING "; }
  if (g_replay_p2_valid) { g_b.bridge.setReplayPage2Data(g_replay_p2); b += "page2=loaded "; }
  else                   { b += "page2=MISSING "; }
  b += "\n# (templates include the original CRC trailer — replay will forward the bytes unchanged)\n";
  server.send(200, "text/plain", b);
}

// /replay?on=0|1 — enable/disable byte-for-byte playback of the snapshotted templates over
// the NEO->board direction. Requires bridge mode=inject to actually take effect.
static void handleReplay() {
  bool on = !server.hasArg("on") || server.arg("on") != "0";
  g_b.bridge.setReplayEnabled(on);
  String r = "# replay ";
  r += on ? "ENABLED" : "disabled";
  r += " (p1=";
  r += g_b.bridge.replayPage1Loaded() ? "loaded" : "MISSING";
  r += " p2=";
  r += g_b.bridge.replayPage2Loaded() ? "loaded" : "MISSING";
  r += ")\n";
  if (on && g_bridge_mode != bridge::StreamingBridge::INJECT)
    r += "# NOTE: bridge mode is " + String(bridgeModeStr(g_bridge_mode)) +
         "; replay only takes effect once you GET /bridge?mode=inject\n";
  server.send(200, "text/plain", r);
}

// /pulse?rules=reg:val,reg:val,...&n=<frames> — arm a transient set of substitution rules
// that fire for the next N recognised B frames (NEO func-03 responses) on the NEO->board
// direction, then auto-expire. Pulse rules take precedence over persistent /inject rules.
// Used to test the page-2 commit-signal hypothesis: zone setpoints commit when a "high-byte-
// to-zero" pulse fires on reg 243 (page-2) + reg 65, 67 (page-1) for one cycle.
//
// Example: pulse all three commit markers for 1 cycle =
//   /pulse?rules=65:0x0032,67:0x0023,243:0x0028&n=1
static void handlePulse() {
  if (g_bridge_mode != bridge::StreamingBridge::INJECT) {
    server.send(409, "text/plain",
      "# REJECTED: bridge mode must be INJECT for /pulse to take effect. Run /bridge?mode=inject first.\n");
    return;
  }
  bridge::Rule rules[bridge::StreamingBridge::MAX_RULES];
  size_t n = 0;
  if (server.hasArg("rules")) {
    String s = server.arg("rules");
    int start = 0;
    while (start < (int)s.length() && n < bridge::StreamingBridge::MAX_RULES) {
      int comma = s.indexOf(',', start);
      String pair = (comma < 0) ? s.substring(start) : s.substring(start, comma);
      int colon = pair.indexOf(':');
      if (colon > 0) {
        rules[n].reg   = (uint16_t)strtoul(pair.substring(0, colon).c_str(), nullptr, 0);
        rules[n].value = (uint16_t)strtoul(pair.substring(colon + 1).c_str(), nullptr, 0);
        n++;
      }
      if (comma < 0) break;
      start = comma + 1;
    }
  }
  size_t n_frames = server.hasArg("n") ? strtoul(server.arg("n").c_str(), nullptr, 0) : 1;
  if (n_frames == 0) n_frames = 1;
  if (n_frames > 100) n_frames = 100;

  g_b.bridge.setPulse(rules, n, n_frames);

  String b = "# pulse ARMED: " + String((unsigned)n) + " rule(s) for next " +
             String((unsigned)n_frames) + " recognised B frame(s)\n";
  for (size_t i = 0; i < n; i++)
    b += "#   reg " + String(rules[i].reg) + " <- 0x" + String(rules[i].value, HEX) +
         " (" + String(rules[i].value) + ")\n";
  server.send(200, "text/plain", b);
}

// /scramble?reg=<n>&on=0|1 — arm/disarm per-frame rule update on the NEO->board direction.
// When armed (reg != 0), every new B frame increments g_scramble_nonce and upserts a rule
// `reg -> nonce` into g_b.bridge. Tests whether the board's commit gate for page-2 values
// is "must differ from last cycle" rather than a deterministic hash.
static void handleScramble() {
  bool on = !server.hasArg("on") || server.arg("on") != "0";
  if (!on) {
    g_scramble_reg = 0;
    server.send(200, "text/plain", "# scramble disabled\n");
    return;
  }
  if (!server.hasArg("reg")) {
    server.send(400, "text/plain", "# need ?reg=<register> (and optional &on=0 to disable)\n");
    return;
  }
  uint16_t r = (uint16_t)strtoul(server.arg("reg").c_str(), nullptr, 0);
  if (r == 0) {
    server.send(400, "text/plain", "# reg=0 means disabled — use ?on=0 to disable explicitly\n");
    return;
  }
  g_scramble_reg = r;
  String b = "# scramble armed: reg " + String(r) +
             " will be rewritten with a fresh nonce on every new B (NEO->board) frame\n";
  b += "# nonce seed=0x" + String(g_scramble_nonce, HEX) + " step=0x9E37\n";
  if (g_bridge_mode != bridge::StreamingBridge::INJECT)
    b += "# NOTE: bridge mode is " + String(bridgeModeStr(g_bridge_mode)) +
         "; scramble only takes effect once you POST /bridge?mode=inject\n";
  server.send(200, "text/plain", b);
}

// /loopback?n=<bytes> — bench test for two-UART operation. Two supported wiring layouts:
//   (a) Pure UART: jumpers GPIO16<->GPIO1 and GPIO17<->GPIO0 (no transceivers).
//   (b) Through transceivers on a shared bus: both modules' A<->A, B<->B. This exercises the
//       full TTL<->RS485<->TTL path. NOTE: auto-direction transceivers echo each sender's TX
//       back to its OWN RX (the transmitter's RO follows the bus, which it's driving) — so
//       we explicitly drain the sender's RX after each phase and don't compare it.
// Sends a deterministic pattern in BOTH directions, sequentially (the shared bus is
// half-duplex), and reports byte integrity on the RECEIVING UART for each direction.
static void handleLoopback() {
  if (g_bridge_mode != bridge::StreamingBridge::OFF || g_write_armed) {
    server.send(409, "text/plain",
      "# REJECTED: bridge or 0x67 emulator is active. /disarm first.\n");
    return;
  }
  size_t n = server.hasArg("n") ? strtoul(server.arg("n").c_str(), nullptr, 10) : 256;
  if (n == 0 || n > 1024) n = 256;

  g_capture = false;   // stop the pumpCapture loop reading from either UART
  delay(50);
  auto drain = [](BusEndpoint& ep) {
    uint32_t until = millis() + 20;
    while (millis() < until) { while (ep.uart.available()) ep.uart.read(); delay(1); }
  };
  drain(g_a);
  drain(g_b);

  uint8_t pattern[1024];
  for (size_t i = 0; i < n; i++) pattern[i] = (uint8_t)(i ^ 0x5A);

  // Phase 1: A -> B. Receiver = UART0 (g_b). Drain g_a's RX afterwards (it contains echo).
  g_a.uart.write(pattern, n);
  g_a.uart.flush();
  uint32_t deadline = millis() + 1000;
  std::vector<uint8_t> recv_b; recv_b.reserve(n);
  while (millis() < deadline && recv_b.size() < n) {
    while (g_b.uart.available() && recv_b.size() < n) recv_b.push_back((uint8_t)g_b.uart.read());
  }
  size_t miss_b = (recv_b.size() != n) ? (n - recv_b.size()) : 0;
  size_t err_b = 0;
  for (size_t i = 0; i < recv_b.size(); i++) if (recv_b[i] != pattern[i]) err_b++;
  drain(g_a);  // discard the transmit echo on the sender's own RX
  drain(g_b);  // and any stragglers on the receiver

  // Phase 2: B -> A. Receiver = UART1 (g_a).
  g_b.uart.write(pattern, n);
  g_b.uart.flush();
  deadline = millis() + 1000;
  std::vector<uint8_t> recv_a; recv_a.reserve(n);
  while (millis() < deadline && recv_a.size() < n) {
    while (g_a.uart.available() && recv_a.size() < n) recv_a.push_back((uint8_t)g_a.uart.read());
  }
  size_t miss_a = (recv_a.size() != n) ? (n - recv_a.size()) : 0;
  size_t err_a = 0;
  for (size_t i = 0; i < recv_a.size(); i++) if (recv_a[i] != pattern[i]) err_a++;
  drain(g_a);
  drain(g_b);

  g_capture = true;

  String b = "# loopback n=" + String((unsigned)n) + "\n";
  b += "# A->B: received=" + String((unsigned)recv_b.size()) + " missing=" + String((unsigned)miss_b) +
       " mismatched=" + String((unsigned)err_b) + "\n";
  b += "# B->A: received=" + String((unsigned)recv_a.size()) + " missing=" + String((unsigned)miss_a) +
       " mismatched=" + String((unsigned)err_a) + "\n";
  bool ok = (miss_a == 0 && err_a == 0 && miss_b == 0 && err_b == 0);
  b += String("# verdict: ") + (ok ? "PASS" : "FAIL") + "\n";
  server.send(200, "text/plain", b);
}

// /uartcheck — isolated diagnostic for the dual-UART path. Writes a single calibration byte
// (0x55, alternating bits) first on UART1 alone, then on UART0 alone, and reports per-step
// what each UART's RX FIFO saw. Lets us tell apart:
//   - UART0 not initialised at all (no echo on its own RX)
//   - Transceiver B unpowered / miswired (UART0 echoes but UART1 RX sees nothing)
//   - A/B polarity swapped (transmits happen but the other side decodes garbage)
static void handleUartCheck() {
  if (g_bridge_mode != bridge::StreamingBridge::OFF || g_write_armed) {
    server.send(409, "text/plain", "# REJECTED: bridge or 0x67 emulator active. /disarm first.\n");
    return;
  }
  g_capture = false;
  delay(50);
  auto drain = [](BusEndpoint& ep) {
    uint32_t until = millis() + 20;
    while (millis() < until) { while (ep.uart.available()) ep.uart.read(); delay(1); }
  };
  auto collect = [](BusEndpoint& ep, uint32_t ms) {
    std::vector<uint8_t> v;
    uint32_t until = millis() + ms;
    while (millis() < until) {
      while (ep.uart.available()) v.push_back((uint8_t)ep.uart.read());
      delay(1);
    }
    return v;
  };
  auto hex = [](const std::vector<uint8_t>& v) {
    String s;
    for (auto b : v) { char buf[4]; snprintf(buf, sizeof(buf), "%02X ", b); s += buf; }
    return s;
  };

  drain(g_a); drain(g_b);

  // Step 1: TX one byte on UART1 only.
  uint8_t probe = 0x55;
  g_a.uart.write(&probe, 1);
  g_a.uart.flush();
  auto a_after_tx_a = collect(g_a, 30);   // UART1 own-RX echo (transceiver A self-loop)
  auto b_after_tx_a = collect(g_b, 30);   // UART0 RX from bus (transceiver A -> bus -> B)
  drain(g_a); drain(g_b);

  // Step 2: TX one byte on UART0 only.
  g_b.uart.write(&probe, 1);
  g_b.uart.flush();
  auto b_after_tx_b = collect(g_b, 30);   // UART0 own-RX echo (transceiver B self-loop)
  auto a_after_tx_b = collect(g_a, 30);   // UART1 RX from bus (transceiver B -> bus -> A)
  drain(g_a); drain(g_b);

  g_capture = true;

  String r = "# uartcheck (1-byte probe = 0x55)\n";
  r += "# TX on UART1 (g_a, GPIO16):\n";
  r += "#   UART1 RX (own echo via transceiver A) -> n=" + String((unsigned)a_after_tx_a.size()) +
       " bytes: " + hex(a_after_tx_a) + "\n";
  r += "#   UART0 RX (via bus to transceiver B)   -> n=" + String((unsigned)b_after_tx_a.size()) +
       " bytes: " + hex(b_after_tx_a) + "\n";
  r += "# TX on UART0 (g_b, GPIO1):\n";
  r += "#   UART0 RX (own echo via transceiver B) -> n=" + String((unsigned)b_after_tx_b.size()) +
       " bytes: " + hex(b_after_tx_b) + "\n";
  r += "#   UART1 RX (via bus to transceiver A)   -> n=" + String((unsigned)a_after_tx_b.size()) +
       " bytes: " + hex(a_after_tx_b) + "\n";
  r += "#\n";
  r += "# Interpretation:\n";
  r += "#   own echoes both non-empty -> both UART + transceiver TX paths live.\n";
  r += "#   own echoes ok, cross both empty -> A/B polarity swap or open bus.\n";
  r += "#   only UART1 own echo -> UART0 not initialised or transceiver B unpowered.\n";
  server.send(200, "text/plain", r);
}

// /blink — visual UART diagnostic for staring at the transceivers' TX/RX LEDs. Pulses each
// UART in turn (6 bursts of ~50ms TX activity at 500ms intervals = 3s per UART, 6s total).
// Reports the TX byte count we drove out of each UART; the operator confirms which TX/RX
// LEDs lit on the physical modules.
static void handleBlink() {
  if (g_bridge_mode != bridge::StreamingBridge::OFF || g_write_armed) {
    server.send(409, "text/plain", "# REJECTED: bridge or 0x67 emulator active. /disarm first.\n");
    return;
  }
  g_capture = false;
  delay(50);

  // 50 bytes @ 9600 baud = ~52 ms of continuous TX activity (LED on); then 450 ms idle.
  uint8_t pulse[50];
  for (size_t i = 0; i < sizeof(pulse); i++) pulse[i] = 0x55;

  auto pulse_uart = [&](BusEndpoint& ep) {
    size_t tx_bytes = 0;
    for (int i = 0; i < 6; i++) {
      ep.uart.write(pulse, sizeof(pulse));
      ep.uart.flush();
      tx_bytes += sizeof(pulse);
      // discard whatever arrives during the gap (own echo, peer's bus output)
      uint32_t until = millis() + 450;
      while (millis() < until) {
        while (g_a.uart.available()) g_a.uart.read();
        while (g_b.uart.available()) g_b.uart.read();
        delay(1);
      }
    }
    return tx_bytes;
  };

  size_t tx_a = pulse_uart(g_a);
  size_t tx_b = pulse_uart(g_b);

  while (g_a.uart.available()) g_a.uart.read();
  while (g_b.uart.available()) g_b.uart.read();
  g_capture = true;

  String r = "# blink complete\n";
  r += "# UART1 (g_a, GPIO16 TX): 6 x 50 bytes = " + String((unsigned)tx_a) + " bytes pulsed\n";
  r += "# UART0 (g_b, GPIO1 TX):  6 x 50 bytes = " + String((unsigned)tx_b) + " bytes pulsed\n";
  r += "# Operator: check the transceiver modules' TX/RX LEDs during each 3s window.\n";
  server.send(200, "text/plain", r);
}

static void handleUpdate() {
  if (!server.hasArg("url")) {
    server.send(400, "text/plain", "# need ?url=http://host:port/firmware.bin\n");
    return;
  }
  String url = server.arg("url");
  server.send(200, "text/plain", "# pulling firmware: " + url + "\n");
  Serial.printf("# OTA: pulling %s\n", url.c_str());

  // Free the priority-1 main loop entirely for the OTA download. The bridge task at
  // priority 5 would otherwise preempt httpUpdate on every byte arriving on either UART
  // (more so since the boot default is INJECT now, which adds per-byte CRC work). Even
  // with g_capture=false the task busy-spins when UART buffers fill up. Suspend it
  // outright — the device reboots on success so no resume is needed.
  g_capture = false;
  if (g_bridge_task != nullptr) vTaskSuspend(g_bridge_task);
  // Drain whatever was already in the UART FIFOs so the transceivers don't sit holding
  // a half-frame the board will reject after we come back up.
  while (g_a.uart.available()) g_a.uart.read();
  while (g_b.uart.available()) g_b.uart.read();
  // Force bridge mode to OFF so any code path that runs before the reboot can't try to
  // drive UART1 TX. Matches the long-standing CLAUDE.md recommendation for OTA reliability.
  setBridgeMode(bridge::StreamingBridge::OFF);

  WiFiClient client;
  // Verify the written image's app identity before booting it (OTA app-guard) — refuse a
  // foreign image (e.g. the somfy-sdn firmware) instead of bricking this in-wall controller.
  httpUpdate.rebootOnUpdate(false);
  t_httpUpdate_return r = httpUpdate.update(client, url);

  if (r == HTTP_UPDATE_OK) {
    const esp_partition_t *next = esp_ota_get_boot_partition();
    esp_app_desc_t d{};
    if (next != nullptr && esp_ota_get_partition_description(next, &d) == ESP_OK &&
        strcmp(d.project_name, APP_ID) == 0) {
      Serial.printf("# OTA ok (app=%s ver=%s) — rebooting\n", d.project_name, d.version);
      delay(150);
      ESP.restart();
    } else {
      esp_ota_set_boot_partition(esp_ota_get_running_partition());
      Serial.printf("# OTA REJECTED: image app=\"%s\" != \"%s\" — reverted, not booting\n",
                    d.project_name, APP_ID);
    }
  } else {
    Serial.printf("# OTA failed (%d): %s\n", (int)r, httpUpdate.getLastErrorString().c_str());
  }
  // Either a rejected or failed OTA leaves us running — restore normal operation.
  setBridgeMode(bridge::StreamingBridge::INJECT);
  if (g_bridge_task != nullptr) vTaskResume(g_bridge_task);
  g_capture = true;
}

static void pumpConsole() {
  static char buf[32];
  static size_t len = 0;
  while (Serial.available()) {
    char c = Serial.read();
    if (c == '\r') continue;
    if (c == '\n') {
      buf[len] = '\0';
      char st[600];
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

// Wipe stored WiFi creds and reboot into the SoftAP provisioning portal (replaces the GPIO0
// button the somfy port has — GPIO0 is the NEO UART RX here). Lets the device be moved to a
// different WiFi without USB.
static void handleWifiReset() {
  server.send(200, "text/plain", "# wiping WiFi creds — rebooting into the actron-mitm-XXXX portal\n");
  delay(300);
  wifi_prov::factoryWipe();  // reboots; does not return
}

// Stand up the app-facing surfaces (HTTP API on 80, WS controller API on 8767, ArduinoOTA).
// Called ONLY when WiFi is up (STA connected) — in portal mode wifi_prov owns port 80, so these
// must not bind. mDNS is started by wifi_prov on connect; the RS485 bridge task runs regardless.
static void startAppServer() {
  // Read-only endpoints: GET. Mutating / bus-driving / OTA endpoints: POST — so they can't be
  // triggered by browser prefetch / crawlers and their params don't leak into GET logs.
  server.on("/", HTTP_GET, handleRoot);
  server.on("/stats", HTTP_GET, handleStats);
  server.on("/stats.json", HTTP_GET, handleStatsJson);
  server.on("/log", HTTP_GET, handleLog);
  server.on("/measure", HTTP_GET, handleMeasure);
  server.on("/set", HTTP_POST, handleSet);
  server.on("/clear", HTTP_POST, handleClear);
  server.on("/update", HTTP_POST, handleUpdate);
  server.on("/wifireset", HTTP_POST, handleWifiReset);
  server.on("/armwrite", HTTP_POST, handleArm);
  server.on("/disarm", HTTP_POST, handleDisarm);
  server.on("/txprobe", HTTP_POST, handleTxProbe);
  server.on("/bridge", HTTP_POST, handleBridge);
  server.on("/inject", HTTP_POST, handleInject);
  server.on("/loopback", HTTP_POST, handleLoopback);
  server.on("/uartcheck", HTTP_POST, handleUartCheck);
  server.on("/blink", HTTP_POST, handleBlink);
  server.on("/scramble", HTTP_POST, handleScramble);
  server.on("/pulse", HTTP_POST, handlePulse);
  server.on("/snapshot", HTTP_POST, handleSnapshot);
  server.on("/replay", HTTP_POST, handleReplay);
  server.on("/loadtemplate", HTTP_POST, handleLoadTemplate);
  server.begin();

  // Controller WS API (Home Assistant client) — separate port from the RE HTTP API.
  ws_api::Hooks hooks{};
  hooks.injector = &g_b.bridge;          // NEO->board direction owns the INJECT rules
  hooks.mode_var = &g_bridge_mode;
  hooks.set_mode = setBridgeMode;
  ws_api::begin(8767, hooks);

  ArduinoOTA.setHostname(wifi_prov::hostname());
  ArduinoOTA.begin();
}

void setup() {
  Serial.begin(115200);

#ifdef RGB_BUILTIN
  pinMode(RGB_PWR, OUTPUT);
  digitalWrite(RGB_PWR, HIGH);
#endif

  startBuses();
  // Propagate the boot-default bridge mode to both endpoints' bridge instances. Default
  // is INJECT so the cut-bus MITM relays bytes immediately on power-up AND the WS
  // controller API can issue writes without an HTTP poke first. INJECT with zero rules
  // is functionally equivalent to PASSTHRU (CRC-tracking overhead is ~166 ns/byte at
  // 9600 baud where each byte takes 1.04 ms — invisible).
  g_a.bridge.setMode(g_bridge_mode);
  g_b.bridge.setMode(g_bridge_mode);

  uint32_t t0 = millis();
  while (!Serial && millis() - t0 < 1500) delay(10);
  Serial.println("\n# Actron RS485 tool (sniffer + MITM bridge + WS controller API)");

  // Provision/connect WiFi: NVS creds via STA, or a SoftAP captive portal (actron-mitm-XXXX) if
  // unprovisioned / connect fails. Only stand up the app HTTP+WS servers once we're on the
  // network — in portal mode wifi_prov owns port 80. The RS485 bridge task (spawned below) runs
  // in BOTH modes, so the A/C keeps bridging even while the controller waits to be provisioned.
  if (wifi_prov::begin() == wifi_prov::Status::CONNECTED) {
    startAppServer();
  } else {
    Serial.println("# Unprovisioned — SoftAP portal up; A/C bridge still running.");
  }

  char st[600];
  statusLine(st, sizeof(st));
  Serial.println(st);

  // Spawn the byte-forwarding task. Priority 5 sits above the Arduino loopTask (priority 1)
  // but well below the WiFi/lwIP stack tasks (~18-23), so WiFi keeps working and our pump
  // preempts only the user-space main loop (HTTP / OTA / console).
  xTaskCreate(bridgeTask, "bridge", 8192, nullptr, 5, &g_bridge_task);
}

void loop() {
  wifi_prov::loop();  // captive portal (PORTAL) + GPIO0 button + on-demand reconnect
  pumpConsole();
  if (wifi_prov::isConnected()) {
    ArduinoOTA.handle();
    server.handleClient();
    ws_api::loop();
  }
  // pumpCapture() / the A/C bridge run on their own FreeRTOS task — see bridgeTask() — so the
  // bridge keeps relaying in both CONNECTED and PORTAL modes.
}
