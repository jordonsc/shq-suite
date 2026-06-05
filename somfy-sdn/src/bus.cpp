#include "bus.h"

#include <Arduino.h>
#include <freertos/FreeRTOS.h>
#include <freertos/queue.h>
#include <freertos/semphr.h>
#include <freertos/task.h>

#include <cstring>

namespace bus {

namespace {

// ---- pins / UART (SPEC §4) ----------------------------------------------
constexpr uint8_t PIN_RX = 17;  // transceiver RO/RXD -> board "RX"
constexpr uint8_t PIN_TX = 16;  // transceiver DI/TXD <- board "TX"
HardwareSerial& UART = Serial1;

constexpr uint32_t SLOW_POLL_MS = 2000;       // idle round-robin cadence
constexpr uint32_t IDLE_DEVICE_POLL_MS = 30000;  // per-device slow refresh
constexpr uint32_t MOVING_DEVICE_POLL_MS = 100;
constexpr uint32_t MIN_INTER_MSG_GAP_MS = 25;  // SPEC §5.1
constexpr uint32_t DISCOVERY_RETRY_MS = 30000; // re-sweep cadence while the table is empty

// ---- module state (owned by the bus task) -------------------------------
// Boot default ACTIVE: this device is the sole controller on a dedicated SDN bus, so it should
// poll + accept commands immediately on power-up (HA covers work after a reboot with no manual
// arming). This deviates from SPEC §6.2's conservative LISTEN-default — switch back to LISTEN
// here if the bus ever gains a competing controller (then arm ACTIVE per deployment).
volatile Mode g_mode = Mode::ACTIVE;
Mode g_mode_before_ota = Mode::ACTIVE;
StateChangedCb g_on_changed = nullptr;
devices::DeviceTable g_table;
errlog::ErrorLog g_errlog;
Stats g_stats;

TaskHandle_t g_task = nullptr;
QueueHandle_t g_cmd_queue = nullptr;

// Raw request plumbing (single in-flight /send at a time, guarded by g_raw_mutex).
SemaphoreHandle_t g_raw_mutex = nullptr;
SemaphoreHandle_t g_raw_done = nullptr;
RawResult g_raw_result;

uint32_t g_job_seq = 0;

// Sniffer ring.
SniffEntry g_sniff[SNIFF_RING];
uint32_t g_sniff_seq = 0;
uint32_t g_last_frame_end_us = 0;

// Per-device poll bookkeeping (parallel to the table slots).
uint32_t g_last_poll_ms[devices::MAX_DEVICES] = {0};

// Auto-discovery bookkeeping: one sweep on boot, then retry while the table is empty. Motors
// don't self-announce on SDN, so this is how covers populate after a power-up without manual
// intervention. Once anything is found, polling keeps it alive and we stop auto-sweeping (use
// the explicit `rediscover` to enrol a motor added later).
bool g_boot_discovery_done = false;
uint32_t g_last_discovery_ms = 0;

// Live UART config (mutable for bring-up diagnostics).
uint32_t g_baud = sdn::BAUD_RATE;
char g_parity = 'O';
volatile bool g_reconfig_pending = false;
volatile uint32_t g_reconfig_baud = sdn::BAUD_RATE;
volatile char g_reconfig_parity = 'O';

uint32_t parityConfig(char p) {
  switch (p) {
    case 'E': case 'e': return SERIAL_8E1;
    case 'N': case 'n': return SERIAL_8N1;
    default: return SERIAL_8O1;
  }
}

void startUart(uint32_t baud, char parity) {
  UART.end();
  UART.setRxBufferSize(512);
  UART.begin(baud, parityConfig(parity), PIN_RX, PIN_TX);
  g_baud = baud;
  g_parity = (parity == 'E' || parity == 'e') ? 'E' : (parity == 'N' || parity == 'n') ? 'N' : 'O';
}

// ---- helpers ------------------------------------------------------------

void logErr(errlog::Class c, const uint8_t* addr, const char* msg, const uint8_t* raw,
            size_t raw_len) {
  g_errlog.record(millis(), c, addr, msg, raw, raw_len);
}

void recordSniff(const uint8_t* bytes, size_t len, bool valid) {
  uint32_t now = micros();
  uint32_t s = ++g_sniff_seq;
  SniffEntry& e = g_sniff[(s - 1) % SNIFF_RING];
  e.seq = s;
  e.t_ms = millis();
  e.gap_us = g_last_frame_end_us ? (now - g_last_frame_end_us) : 0;
  e.len = (uint8_t)((len > sdn::MAX_FRAME_LEN) ? sdn::MAX_FRAME_LEN : len);
  e.valid = valid ? 1 : 0;
  memcpy(e.bytes, bytes, e.len);
  g_last_frame_end_us = now;
}

// Extract the motor address from a frame: a motor->tool reply carries the motor in src; a
// tool->motor directed frame carries it in dst. Returns false for broadcast-only frames.
bool motorAddrOf(const sdn::ParsedFrame& pf, uint8_t out[3]) {
  if (pf.network == sdn::NET_MOTOR_TO_TOOL) {
    memcpy(out, pf.src_addr, 3);
    return !sdn::addrIsBroadcast(out);
  }
  memcpy(out, pf.dst_addr, 3);
  return !sdn::addrIsBroadcast(out);
}

bool readByte(uint8_t* b, uint32_t timeout_ms) {
  uint32_t deadline = millis() + timeout_ms;
  while (!UART.available()) {
    if ((int32_t)(millis() - deadline) >= 0) return false;
    vTaskDelay(1);  // blocking, CPU-yielding wait — never busy-spin (SPEC §6.5)
  }
  int v = UART.read();
  if (v < 0) return false;
  *b = (uint8_t)v;
  return true;
}

bool readBytes(uint8_t* buf, size_t n, uint32_t timeout_ms) {
  size_t got = 0;
  uint32_t deadline = millis() + timeout_ms;
  while (got < n) {
    if (UART.available()) {
      int v = UART.read();
      if (v < 0) break;
      buf[got++] = (uint8_t)v;
    } else {
      if ((int32_t)(millis() - deadline) >= 0) break;
      vTaskDelay(1);
    }
  }
  return got == n;
}

void drainRx() {
  while (UART.available()) UART.read();
}

// Read one SDN frame from the wire using its embedded length field. Mirrors the proven
// sdn_receive_frame. Returns total frame length, or 0 on timeout / bad length.
size_t readFrame(uint8_t* buf, uint32_t first_timeout_ms) {
  if (!readByte(&buf[0], first_timeout_ms)) return 0;
  if (!readByte(&buf[1], sdn::INTER_BYTE_TIMEOUT_MS)) {
    logErr(errlog::Class::FRAMING, nullptr, "len byte timeout", buf, 1);
    g_stats.err_framing++;
    return 0;
  }
  uint8_t raw_len = (uint8_t)~buf[1];
  size_t frame_len = raw_len & 0x7F;
  if (frame_len < sdn::HEADER_LEN + sdn::CKSUM_LEN || frame_len > sdn::MAX_FRAME_LEN) {
    logErr(errlog::Class::FRAMING, nullptr, "bad frame length", buf, 2);
    g_stats.err_framing++;
    drainRx();
    return 0;
  }
  if (!readBytes(&buf[2], frame_len - 2, sdn::REPLY_BODY_TIMEOUT_MS)) {
    logErr(errlog::Class::FRAMING, nullptr, "short frame body", buf, 2);
    g_stats.err_framing++;
    return 0;
  }
  return frame_len;
}

bool sendFrame(const uint8_t* buf, size_t len) {
  if (g_mode != Mode::ACTIVE) return false;  // firmware TX gate
  drainRx();  // discard stale bytes / echoes before we transmit
  UART.write(buf, len);
  UART.flush();  // wait TX FIFO drain so the auto-direction transceiver flips back to RX
  g_stats.tx_frames++;
  return true;
}

// Feed a parsed/raw inbound frame into the table + sniffer + errlog. Returns true if any
// tracked state changed.
bool processInbound(const uint8_t* raw, size_t len) {
  sdn::ParsedFrame pf;
  bool valid = sdn::parseFrame(raw, len, &pf);
  recordSniff(raw, len, valid);

  if (!valid) {
    logErr(errlog::Class::CHECKSUM, nullptr, "checksum/parse fail", raw, len);
    g_stats.err_checksum++;
    return false;
  }
  g_stats.rx_frames++;

  uint8_t addr[3];
  if (!motorAddrOf(pf, addr)) return false;  // broadcast-only frame, nothing to attribute

  devices::Device* d = g_table.upsert(addr, devices::Source::OBSERVED, millis());
  if (d == nullptr) return false;
  bool became_online = false;
  g_table.touch(d, millis(), &became_online);
  bool changed = became_online;
  if (became_online) logErr(errlog::Class::ONLINE, addr, "device online", nullptr, 0);

  switch (pf.msg_id) {
    case sdn::MSG_POST_MOTOR_POSITION: {
      sdn::PositionReport pr = sdn::parsePosition(pf.data, pf.data_len);
      devices::PosResult r = g_table.applyPosition(d, pr, millis());
      if (r.became_fault) {
        logErr(r.stalled ? errlog::Class::STALL : errlog::Class::POS_UNKNOWN, addr,
               r.stalled ? "motor stalled" : "position unknown", pf.data, pf.data_len);
      }
      changed |= r.changed;
      break;
    }
    case sdn::MSG_POST_MOTOR_LIMITS: {
      sdn::LimitsReport lr = sdn::parseLimits(pf.data, pf.data_len);
      changed |= g_table.applyLimits(d, lr);
      break;
    }
    case sdn::MSG_POST_MOTOR_DIRECTION: {
      if (pf.data_len >= 1) {
        uint8_t dir = pf.data[0] & 0x01;
        if (!d->direction_known || d->direction != dir) {
          d->direction = dir;
          d->direction_known = true;
          changed = true;
        }
      }
      break;
    }
    case sdn::MSG_NACK: {
      uint8_t code = (pf.data_len >= 1) ? pf.data[0] : 0;
      const char* reason = nullptr;
      switch (code) {  // SDN Nack reason codes
        case 0x22: reason = "rejected: no limits"; break;
        case 0x21: reason = "rejected: position unknown"; break;
        case 0x20: reason = "rejected: locked"; break;
        case 0x24: reason = "rejected: out of range"; break;
        case 0xFF: reason = "rejected: busy"; break;
        default: break;
      }
      if (reason) snprintf(d->status, sizeof(d->status), "%s", reason);
      else snprintf(d->status, sizeof(d->status), "rejected 0x%02X", code);
      logErr(errlog::Class::NACK, addr, "motor NACK", pf.data, pf.data_len);
      g_stats.err_nack++;
      changed = true;
      break;
    }
    default:
      break;  // POST_NODE_ADDR etc. — registration above is enough
  }
  return changed;
}

// Send a tool->motor transaction with retry, parse the reply. Returns true if a valid reply
// was received; *reply holds it. Also feeds the reply through processInbound.
bool transact(uint8_t msg, const uint8_t dst[3], const uint8_t* data, size_t dlen,
              sdn::ParsedFrame* reply, uint8_t* reply_raw, size_t* reply_raw_len) {
  uint8_t tx[sdn::MAX_FRAME_LEN];
  size_t tlen = sdn::buildToolFrame(tx, msg, dst, data, dlen);
  if (tlen == 0) return false;

  for (int attempt = 0; attempt < sdn::RETRY_COUNT; attempt++) {
    if (attempt > 0) vTaskDelay(pdMS_TO_TICKS(sdn::RETRY_DELAY_MS));
    if (!sendFrame(tx, tlen)) return false;  // not ACTIVE

    uint8_t rx[sdn::MAX_FRAME_LEN];
    size_t rlen = readFrame(rx, sdn::RESPONSE_TIMEOUT_MS);
    if (rlen == 0) {
      logErr(errlog::Class::TIMEOUT, sdn::addrIsBroadcast(dst) ? nullptr : dst, "no reply",
             nullptr, 0);
      g_stats.err_timeout++;
      continue;
    }
    bool changed = processInbound(rx, rlen);
    sdn::ParsedFrame pf;
    if (sdn::parseFrame(rx, rlen, &pf)) {
      if (reply) *reply = pf;
      if (reply_raw && reply_raw_len) {
        memcpy(reply_raw, rx, rlen);
        *reply_raw_len = rlen;
      }
      if (changed && g_on_changed) g_on_changed();
      return true;
    }
  }
  return false;
}

void queryPosition(devices::Device* d) {
  g_stats.polls++;
  sdn::ParsedFrame reply;
  transact(sdn::MSG_GET_MOTOR_POSITION, d->addr, nullptr, 0, &reply, nullptr, nullptr);
}

void queryLimits(devices::Device* d) {
  sdn::ParsedFrame reply;
  transact(sdn::MSG_GET_MOTOR_LIMITS, d->addr, nullptr, 0, &reply, nullptr, nullptr);
}

void queryDirection(devices::Device* d) {
  sdn::ParsedFrame reply;
  transact(sdn::MSG_GET_MOTOR_DIRECTION, d->addr, nullptr, 0, &reply, nullptr, nullptr);
}

// Discovery sweep (SPEC §6.3). Best-effort on a multi-motor bus (simultaneous replies
// collide); reliable for one-at-a-time enrolment. Ported from sdn_discover_motor.
void discover() {
  if (g_mode != Mode::ACTIVE) return;
  uint8_t bc[3];
  memcpy(bc, sdn::BROADCAST_ADDR, 3);

  // Enter discovery mode.
  uint8_t disc_start[1] = {0x00};
  uint8_t tx[sdn::MAX_FRAME_LEN];
  size_t tlen = sdn::buildToolFrame(tx, sdn::MSG_SET_NODE_DISCOVERY, bc, disc_start, 1);
  sendFrame(tx, tlen);
  vTaskDelay(pdMS_TO_TICKS(200));

  for (int attempt = 0; attempt < 3; attempt++) {
    sdn::ParsedFrame reply;
    if (transact(sdn::MSG_GET_NODE_ADDR, bc, nullptr, 0, &reply, nullptr, nullptr)) {
      uint8_t addr[3];
      memcpy(addr, reply.src_addr, 3);
      if (!sdn::addrIsBroadcast(addr)) {
        devices::Device* d = g_table.upsert(addr, devices::Source::DISCOVERED, millis());
        if (d) {
          g_table.touch(d, millis(), nullptr);
          // Acknowledge discovery to the motor.
          uint8_t ack[1] = {0x01};
          sdn::ParsedFrame ar;
          transact(sdn::MSG_SET_NODE_DISCOVERY, addr, ack, 1, &ar, nullptr, nullptr);
        }
      }
    }
    vTaskDelay(pdMS_TO_TICKS(100));
  }

  // Exit discovery mode.
  tlen = sdn::buildToolFrame(tx, sdn::MSG_SET_NODE_DISCOVERY, bc, disc_start, 1);
  sendFrame(tx, tlen);
  if (g_on_changed) g_on_changed();
}

// Execute one dequeued command. Builds + sends the appropriate frame.
void execCommand(const Command& c) {
  uint8_t data[sdn::MAX_FRAME_LEN];
  size_t dlen = 0;
  uint8_t msg = 0;
  const uint8_t* dst = c.broadcast ? sdn::BROADCAST_ADDR : c.addr;
  devices::Device* d = c.broadcast ? nullptr : g_table.find(c.addr);

  switch (c.type) {
    case CmdType::REDISCOVER:
      discover();
      return;

    case CmdType::FORGET:
      // Local table removal (e.g. a swapped-out motor). No bus traffic. Note: if the forgotten
      // motor is the only one and is still live, the while-empty auto-discovery will re-add it.
      if (g_table.remove(c.addr) && g_on_changed) g_on_changed();
      return;

    case CmdType::PROBE: {
      // Force a broadcast GET_NODE_ADDR onto the wire (ignore the TX gate — explicit operator
      // probe) and capture every raw byte that comes back within c.u16 ms.
      g_raw_result = RawResult{};
      uint8_t tx[sdn::MAX_FRAME_LEN];
      size_t tlen = sdn::buildToolFrame(tx, sdn::MSG_GET_NODE_ADDR, sdn::BROADCAST_ADDR, nullptr, 0);
      drainRx();
      UART.write(tx, tlen);
      UART.flush();
      g_stats.tx_frames++;
      g_raw_result.sent = true;
      size_t n = 0;
      uint32_t deadline = millis() + (c.u16 ? c.u16 : 400);
      while (n < sizeof(g_raw_result.raw)) {
        if (UART.available()) {
          int v = UART.read();
          if (v < 0) break;
          g_raw_result.raw[n++] = (uint8_t)v;
        } else {
          if ((int32_t)(millis() - deadline) >= 0) break;
          vTaskDelay(1);
        }
      }
      g_raw_result.raw_len = n;
      g_raw_result.got_reply = (n > 0);
      if (g_raw_done) xSemaphoreGive(g_raw_done);
      return;
    }

    case CmdType::RAW: {
      // Capture the reply for the /send path.
      g_raw_result = RawResult{};
      if (g_mode != Mode::ACTIVE) {
        g_raw_result.sent = false;
        if (g_raw_done) xSemaphoreGive(g_raw_done);
        return;
      }
      sdn::ParsedFrame reply;
      uint8_t reply_raw[sdn::MAX_FRAME_LEN];
      size_t reply_raw_len = 0;
      g_raw_result.sent = true;
      bool ok = transact(c.raw_msg, dst, c.raw_data, c.raw_len, &reply, reply_raw,
                         &reply_raw_len);
      if (ok) {
        g_raw_result.got_reply = true;
        g_raw_result.reply = reply;
        memcpy(g_raw_result.raw, reply_raw, reply_raw_len);
        g_raw_result.raw_len = reply_raw_len;
      }
      if (g_raw_done) xSemaphoreGive(g_raw_done);
      return;
    }

    case CmdType::OPEN:
      msg = sdn::MSG_CTRL_MOVETO;
      dlen = sdn::payloadMoveTo(data, sdn::MOVETO_UP_LIMIT, 0);
      if (d) g_table.beginMove(d, sdn::MovementState::MOVING_UP, 0);
      break;
    case CmdType::CLOSE:
      msg = sdn::MSG_CTRL_MOVETO;
      dlen = sdn::payloadMoveTo(data, sdn::MOVETO_DOWN_LIMIT, 0);
      if (d) g_table.beginMove(d, sdn::MovementState::MOVING_DOWN, 100);
      break;
    case CmdType::STOP:
      msg = sdn::MSG_CTRL_STOP;
      dlen = sdn::payloadStop(data);
      if (d) g_table.beginMove(d, sdn::MovementState::IDLE, d->position_pct);
      break;
    case CmdType::SET_POSITION: {
      msg = sdn::MSG_CTRL_MOVETO;
      uint8_t pct = c.u8 > 100 ? 100 : c.u8;  // native Somfy %
      dlen = sdn::payloadMoveTo(data, sdn::MOVETO_PERCENT, pct);
      if (d) {
        sdn::MovementState dir = (pct > d->position_pct) ? sdn::MovementState::MOVING_DOWN
                                                         : sdn::MovementState::MOVING_UP;
        g_table.beginMove(d, dir, pct);
      }
      break;
    }
    case CmdType::MOVE_STEPS:
      msg = sdn::MSG_CTRL_MOVEOF;
      dlen = sdn::payloadMoveOf(data, c.u8, c.u16);
      break;
    case CmdType::MOVE_TIMED:
      // Momentary commissioning jog — no target/movement bookkeeping (it auto-stops after the
      // duration, and a no-limit motor has no position to track / would false-trip the stall).
      msg = sdn::MSG_CTRL_MOVE;
      dlen = sdn::payloadMove(data, c.u8 ? sdn::MOVE_DIR_UP : sdn::MOVE_DIR_DOWN,
                              (uint8_t)c.u16, sdn::MOVE_SPEED_NORMAL);
      break;
    case CmdType::SET_TOP_LIMIT:
      msg = sdn::MSG_SET_MOTOR_LIMITS;
      dlen = sdn::payloadSetLimit(data, sdn::LIMIT_FN_AT_CURRENT, sdn::LIMIT_DIR_UP, 0);
      break;
    case CmdType::SET_BOTTOM_LIMIT:
      msg = sdn::MSG_SET_MOTOR_LIMITS;
      dlen = sdn::payloadSetLimit(data, sdn::LIMIT_FN_AT_CURRENT, sdn::LIMIT_DIR_DOWN, 0);
      break;
    case CmdType::SET_BOTTOM_LIMIT_AT:
      // Bottom limit at an absolute pulse count from the top (0) reference. Doesn't move the
      // motor; just records the limit.
      msg = sdn::MSG_SET_MOTOR_LIMITS;
      dlen = sdn::payloadSetLimit(data, sdn::LIMIT_FN_ABSOLUTE, sdn::LIMIT_DIR_DOWN, c.u16);
      break;
    case CmdType::SET_DIRECTION:
      msg = sdn::MSG_SET_MOTOR_DIRECTION;
      dlen = sdn::payloadSetDirection(data, c.u8 ? sdn::DIRECTION_REVERSED
                                                 : sdn::DIRECTION_STANDARD);
      break;
    case CmdType::RESET:
      msg = sdn::MSG_SET_FACTORY_DEFAULT;
      dlen = sdn::payloadFactoryDefault(data, c.u8 ? c.u8 : sdn::RESET_LIMITS);
      break;
    case CmdType::IDENTIFY:
      msg = sdn::MSG_CTRL_WINK;
      dlen = 0;
      break;
  }

  sdn::ParsedFrame reply;
  transact(msg, dst, dlen ? data : nullptr, dlen, &reply, nullptr, nullptr);

  // After a limit-changing command, re-read the motor's actual limits (and position after a
  // reset) so the cache + HA reflect reality immediately instead of showing stale values.
  if (d != nullptr) {
    if (c.type == CmdType::RESET) {
      d->position_known = false;
      queryLimits(d);
      queryPosition(d);
    } else if (c.type == CmdType::SET_TOP_LIMIT || c.type == CmdType::SET_BOTTOM_LIMIT ||
               c.type == CmdType::SET_BOTTOM_LIMIT_AT) {
      queryLimits(d);
      queryPosition(d);
    }
  }
  if (g_on_changed) g_on_changed();
}

bool anyMoving() {
  for (size_t i = 0; i < g_table.count(); i++) {
    devices::Device* d = g_table.at(i);
    if (d && d->movement != sdn::MovementState::IDLE) return true;
  }
  return false;
}

// Poll due devices (ACTIVE only). Moving devices polled fast; idle devices on a slow refresh.
void pollCycle() {
  uint32_t now = millis();
  for (size_t i = 0; i < g_table.count(); i++) {
    devices::Device* d = g_table.at(i);
    if (d == nullptr) continue;
    uint32_t interval =
        (d->movement != sdn::MovementState::IDLE) ? MOVING_DEVICE_POLL_MS : IDLE_DEVICE_POLL_MS;
    // Poll a never-polled device immediately (==0) so a just-discovered motor reports position
    // promptly after boot rather than waiting a full idle interval.
    if (g_last_poll_ms[i] != 0 && (uint32_t)(now - g_last_poll_ms[i]) < interval) continue;
    g_last_poll_ms[i] = now;
    if (!d->limits_known) queryLimits(d);
    if (!d->direction_known) queryDirection(d);  // static; read once so HA shows normal/reversed
    queryPosition(d);
  }
}

void busTask(void* /*arg*/) {
  for (;;) {
    // 0. Apply a pending UART reconfigure (bring-up diagnostics).
    if (g_reconfig_pending) {
      g_reconfig_pending = false;
      startUart(g_reconfig_baud, g_reconfig_parity);
    }

    // 1. Drain queued commands.
    Command c;
    while (xQueueReceive(g_cmd_queue, &c, 0) == pdTRUE) {
      execCommand(c);
    }

    // 2. Passive read — catch any frames on the wire (third-party controller / Set Pro in
    //    LISTEN, or unsolicited motor frames in ACTIVE). Short, yielding reads.
    for (int i = 0; i < 4; i++) {
      uint8_t rx[sdn::MAX_FRAME_LEN];
      size_t rlen = readFrame(rx, 30);
      if (rlen == 0) break;
      bool changed = processInbound(rx, rlen);
      if (changed && g_on_changed) g_on_changed();
    }

    // 3. Active discovery: one sweep on boot, then retry while the table stays empty.
    if (g_mode == Mode::ACTIVE) {
      uint32_t now = millis();
      if (!g_boot_discovery_done) {
        g_boot_discovery_done = true;
        g_last_discovery_ms = now;
        discover();
      } else if (g_table.count() == 0 &&
                 (uint32_t)(now - g_last_discovery_ms) >= DISCOVERY_RETRY_MS) {
        g_last_discovery_ms = now;
        discover();
      }
    }

    // 4. Active polling.
    if (g_mode == Mode::ACTIVE) pollCycle();

    // 4. Comms-loss sweep.
    devices::Device* offline[devices::MAX_DEVICES];
    size_t n = g_table.sweepOffline(millis(), devices::DEFAULT_OFFLINE_MS, offline,
                                    devices::MAX_DEVICES);
    if (n > 0) {
      for (size_t i = 0; i < n; i++) {
        logErr(errlog::Class::OFFLINE, offline[i]->addr, "device offline", nullptr, 0);
      }
      if (g_on_changed) g_on_changed();
    }

    // 5. Wait — wake immediately on a new command (xTaskNotifyGive from enqueue).
    uint32_t wait = anyMoving() ? MOVING_DEVICE_POLL_MS : SLOW_POLL_MS;
    ulTaskNotifyTake(pdTRUE, pdMS_TO_TICKS(wait));
  }
}

}  // namespace

// ---- public API ----------------------------------------------------------

void begin(StateChangedCb on_state_changed) {
  g_on_changed = on_state_changed;
  g_cmd_queue = xQueueCreate(16, sizeof(Command));
  g_raw_mutex = xSemaphoreCreateMutex();
  g_raw_done = xSemaphoreCreateBinary();

  startUart(sdn::BAUD_RATE, 'O');

  // Priority above the Arduino loopTask (1) but below the WiFi/lwIP stack — same split as
  // Actron's bridgeTask. 8 KB stack to match.
  xTaskCreate(busTask, "sdn_bus", 8192, nullptr, 5, &g_task);
}

void setMode(Mode m) { g_mode = m; }
Mode mode() { return g_mode; }

uint32_t nextJobId() { return ++g_job_seq; }

bool enqueue(const Command& c) {
  if (g_cmd_queue == nullptr) return false;
  if (xQueueSend(g_cmd_queue, &c, 0) != pdTRUE) return false;
  if (g_task) xTaskNotifyGive(g_task);  // wake the task immediately
  return true;
}

bool requestRaw(const uint8_t addr[3], bool broadcast, uint8_t msg, const uint8_t* data,
                size_t len, RawResult* out, uint32_t timeout_ms) {
  if (g_raw_mutex == nullptr) return false;
  if (xSemaphoreTake(g_raw_mutex, pdMS_TO_TICKS(timeout_ms)) != pdTRUE) return false;

  // Clear any stale completion signal.
  xSemaphoreTake(g_raw_done, 0);

  Command c;
  c.type = CmdType::RAW;
  c.broadcast = broadcast;
  if (!broadcast) memcpy(c.addr, addr, 3);
  c.raw_msg = msg;
  c.raw_len = (uint8_t)((len > RAW_DATA_MAX) ? RAW_DATA_MAX : len);
  if (data && c.raw_len) memcpy(c.raw_data, data, c.raw_len);
  c.job_id = nextJobId();

  bool ok = false;
  if (enqueue(c)) {
    if (xSemaphoreTake(g_raw_done, pdMS_TO_TICKS(timeout_ms)) == pdTRUE) {
      if (out) *out = g_raw_result;
      ok = true;
    }
  }
  xSemaphoreGive(g_raw_mutex);
  return ok;
}

void addConfiguredDevice(const uint8_t addr[3]) {
  g_table.upsert(addr, devices::Source::CONFIGURED, millis());
}

devices::DeviceTable& table() { return g_table; }
errlog::ErrorLog& errors() { return g_errlog; }
const Stats& stats() { return g_stats; }

size_t sniffSince(uint32_t since, SniffEntry* out, size_t max, uint32_t* seq_max) {
  if (seq_max) *seq_max = g_sniff_seq;
  uint32_t maxseq = g_sniff_seq;
  uint32_t oldest = (maxseq > SNIFF_RING) ? (maxseq - SNIFF_RING + 1) : 1;
  uint32_t start = since + 1;
  if (start < oldest) start = oldest;
  size_t n = 0;
  for (uint32_t s = start; s <= maxseq && n < max; s++) {
    SniffEntry& e = g_sniff[(s - 1) % SNIFF_RING];
    if (e.seq != s) continue;
    out[n++] = e;
  }
  return n;
}

void clearDiagnostics() {
  g_errlog.clear();
  g_sniff_seq = 0;
  for (size_t i = 0; i < SNIFF_RING; i++) g_sniff[i] = SniffEntry{};
  g_stats = Stats{};
}

void reconfigure(uint32_t baud, char parity) {
  g_reconfig_baud = baud;
  g_reconfig_parity = parity;
  g_reconfig_pending = true;
  if (g_task) xTaskNotifyGive(g_task);
}

size_t rawProbe(uint8_t* out, size_t max, uint32_t ms) {
  if (g_raw_mutex == nullptr) return 0;
  if (xSemaphoreTake(g_raw_mutex, pdMS_TO_TICKS(ms + 1000)) != pdTRUE) return 0;
  xSemaphoreTake(g_raw_done, 0);
  Command c;
  c.type = CmdType::PROBE;
  c.u16 = (uint16_t)ms;
  c.job_id = nextJobId();
  size_t n = 0;
  if (enqueue(c) && xSemaphoreTake(g_raw_done, pdMS_TO_TICKS(ms + 1000)) == pdTRUE) {
    n = g_raw_result.raw_len;
    if (n > max) n = max;
    if (out) memcpy(out, g_raw_result.raw, n);
  }
  xSemaphoreGive(g_raw_mutex);
  return n;
}

uint32_t currentBaud() { return g_baud; }
char currentParity() { return g_parity; }

void otaSuspend() {
  g_mode_before_ota = g_mode;       // restore on a failed update (success reboots)
  g_mode = Mode::LISTEN;            // force TX off
  if (g_task) vTaskSuspend(g_task); // stop the task touching the UART
  drainRx();                        // clear the RX FIFO so nothing wakes mid-flash
}

void otaResume() {
  g_mode = g_mode_before_ota;
  if (g_task) vTaskResume(g_task);
}

}  // namespace bus
