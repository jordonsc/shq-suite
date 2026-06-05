// SDN bus manager — owns UART1 on a dedicated FreeRTOS task (SPEC §6.2 / §6.5).
//
// This is the ONLY code that touches UART1. WS/HTTP handlers enqueue commands; the task
// serialises all bus access (no concurrent TX), runs request/response transactions with
// retry, polls motor positions, and passively decodes every frame it sees (ours or a third
// party's) into the device table + sniffer ring + error log. Boot default is LISTEN
// (firmware-gated TX safety); nothing is transmitted until armed to ACTIVE.
//
// Concurrency discipline (the Actron OTA-brick lesson, SPEC §6.5): the task spends its time
// in blocking, CPU-yielding waits (uart reads with timeouts + a notification-wait between
// transactions) — never a busy-poll. otaSuspend() must be called before httpUpdate(): it
// suspends the task, drains the RX FIFO, and forces LISTEN so no UART traffic can backlog
// during the flash.

#pragma once

#include <cstddef>
#include <cstdint>

#include "devices.h"
#include "errlog.h"
#include "sdn.h"

namespace bus {

enum class Mode : uint8_t { LISTEN = 0, ACTIVE = 1 };

enum class CmdType : uint8_t {
  OPEN,            // CTRL_MOVETO up limit
  CLOSE,           // CTRL_MOVETO down limit
  STOP,            // CTRL_STOP
  SET_POSITION,    // CTRL_MOVETO percent (u8 = native Somfy %)
  MOVE_STEPS,      // CTRL_MOVEOF (u8 = fn, u16 = param) — relative, needs limits set
  MOVE_TIMED,      // CTRL_MOVE momentary nudge (u8 = 1 up/0 down, u16 = duration) — commissioning
  SET_TOP_LIMIT,   // SET_MOTOR_LIMITS at current pos, up
  SET_BOTTOM_LIMIT,// SET_MOTOR_LIMITS at current pos, down
  SET_BOTTOM_LIMIT_AT, // SET_MOTOR_LIMITS at an absolute pulse count, down (u16 = pulses)
  SET_DIRECTION,   // SET_MOTOR_DIRECTION (u8 = dir)
  RESET,           // SET_FACTORY_DEFAULT (u8 = reset scope, e.g. sdn::RESET_LIMITS)
  IDENTIFY,        // CTRL_WINK
  FORGET,          // remove a motor from the device table (local; no bus traffic)
  REDISCOVER,      // discovery sweep
  RAW,             // raw frame build+send, capture reply (the /send RE workhorse)
  PROBE,           // bring-up diagnostic: TX a broadcast GET_NODE_ADDR, capture RAW rx bytes
};

constexpr size_t RAW_DATA_MAX = sdn::MAX_FRAME_LEN;

struct Command {
  CmdType type = CmdType::STOP;
  uint8_t addr[3] = {0, 0, 0};
  bool broadcast = false;
  uint8_t u8 = 0;
  uint16_t u16 = 0;
  uint8_t raw_msg = 0;
  uint8_t raw_data[RAW_DATA_MAX] = {0};
  uint8_t raw_len = 0;
  uint32_t job_id = 0;
};

struct Stats {
  uint32_t tx_frames = 0;
  uint32_t rx_frames = 0;
  uint32_t polls = 0;
  uint32_t err_checksum = 0;
  uint32_t err_framing = 0;
  uint32_t err_timeout = 0;
  uint32_t err_nack = 0;
};

// Result captured for a RAW request (the /send path).
struct RawResult {
  bool sent = false;        // we transmitted (false if not in ACTIVE mode)
  bool got_reply = false;
  sdn::ParsedFrame reply;
  uint8_t raw[sdn::MAX_FRAME_LEN] = {0};  // un-parsed reply bytes as seen on the wire
  size_t raw_len = 0;
};

// Sniffer ring entry (recent frames seen on the bus, any direction).
struct SniffEntry {
  uint32_t seq = 0;     // monotonic, 1-based; 0 = empty
  uint32_t t_ms = 0;
  uint32_t gap_us = 0;
  uint8_t len = 0;
  uint8_t valid = 0;    // parsed + checksum OK
  uint8_t bytes[sdn::MAX_FRAME_LEN] = {0};
};
constexpr size_t SNIFF_RING = 64;

using StateChangedCb = void (*)();

// One-time init: configure UART1 (4800 8-O-1, auto-direction RS485) and spawn the bus task.
void begin(StateChangedCb on_state_changed);

// TX gate.
void setMode(Mode m);
Mode mode();

// Enqueue a command (thread-safe). For non-RAW jobs returns true if queued. RAW jobs should
// use requestRaw() instead.
bool enqueue(const Command& c);
uint32_t nextJobId();

// Synchronous raw request for /send: builds a frame, transmits (requires ACTIVE), waits up to
// timeout_ms for a reply, and fills `out`. Returns false on queue/timeout failure. Safe to
// call from the HTTP handler — it does NOT touch the UART, it round-trips through the bus task.
bool requestRaw(const uint8_t addr[3], bool broadcast, uint8_t msg, const uint8_t* data,
                size_t len, RawResult* out, uint32_t timeout_ms);

// Pre-populate the table with a known motor address (CONFIGURED source — most reliable on a
// populated bus). Call before/after begin(); thread-safe-ish (used at boot from NVS).
void addConfiguredDevice(const uint8_t addr[3]);

// Accessors — read from the main loop (WS/HTTP). The bus task owns the data; like Actron we
// accept the benign single-core read/write interleave for serialisation snapshots.
devices::DeviceTable& table();
errlog::ErrorLog& errors();
const Stats& stats();

// Copy up to `max` newest-first sniffer entries with seq > since. Returns count copied and
// sets *seq_max to the current high-water mark.
size_t sniffSince(uint32_t since, SniffEntry* out, size_t max, uint32_t* seq_max);
void clearDiagnostics();

// Bring-up diagnostics. reconfigure() re-inits UART1 at a new baud/parity ('N'|'E'|'O') —
// applied by the bus task. rawProbe() forces a broadcast GET_NODE_ADDR onto the wire
// (regardless of LISTEN/ACTIVE) and captures whatever raw bytes come back within ms (our own
// echo on a self-echoing transceiver, or a motor reply). Returns bytes captured.
void reconfigure(uint32_t baud, char parity);
size_t rawProbe(uint8_t* out, size_t max, uint32_t ms);
uint32_t currentBaud();
char currentParity();

// OTA teardown (SPEC §6.5) — MUST run before httpUpdate(): suspend the task, drain the RX
// FIFO, force LISTEN. otaResume() restores on a failed update (success reboots).
void otaSuspend();
void otaResume();

}  // namespace bus
