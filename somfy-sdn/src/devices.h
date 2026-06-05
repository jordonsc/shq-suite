// Per-motor device table keyed by SDN node address (SPEC §6.3). Pure C++ (depends only on
// sdn.h) so the table logic — registration, position/limit application, stall + fault
// detection, comms-loss transitions — is host-unit-testable. The Arduino bus task owns an
// instance and feeds it parsed frames; ws_api / http_api read snapshots from it.
//
// Firmware state is kept in NATIVE Somfy percent (0 = open .. 100 = closed) throughout; the
// HA inversion happens only at the WS boundary via sdn::somfyToHa (SPEC §5.5).

#pragma once

#include <cstddef>
#include <cstdint>

#include "sdn.h"

namespace devices {

constexpr size_t MAX_DEVICES = 16;
constexpr size_t LABEL_LEN = 17;
constexpr uint32_t DEFAULT_OFFLINE_MS = 90000;  // comms-loss after this long without a frame
constexpr uint8_t STALL_THRESHOLD = 20;         // polls with no change while moving (~2s @100ms)

// How a device first entered the table (priority order in SPEC §6.3).
enum class Source : uint8_t { CONFIGURED = 0, DISCOVERED = 1, OBSERVED = 2 };

struct Device {
  bool used = false;
  uint8_t addr[3] = {0, 0, 0};   // raw (byte-reversed) node address
  char label[LABEL_LEN] = {0};
  Source source = Source::OBSERVED;

  uint8_t position_pct = 0;      // native Somfy %, 0=open .. 100=closed
  uint16_t position_pulses = 0;
  bool position_known = false;

  uint16_t up_limit_pulses = 0;
  uint16_t down_limit_pulses = 0;
  bool limits_known = false;

  uint8_t direction = sdn::DIRECTION_STANDARD;  // 0 std / 1 reversed
  bool direction_known = false;

  sdn::MovementState movement = sdn::MovementState::IDLE;
  uint8_t target_pct = 0;        // native Somfy % target for an in-flight move
  bool fault = false;            // position unknown / stalled
  uint8_t stall_count = 0;

  uint32_t last_seen_ms = 0;     // for comms-loss detection
  bool online = false;

  char status[24] = {0};         // human-readable last-significant-event (fault/NACK reason);
                                 // empty = ok. Surfaced to HA so problems are visible.
};

// Result of applying a position report — the caller (bus.cpp) decides what to log/publish.
struct PosResult {
  bool changed = false;           // any tracked field moved
  bool became_fault = false;      // newly entered fault (POS_UNKNOWN / stall)
  bool cleared_fault = false;
  bool stalled = false;           // stall threshold tripped this call
  bool movement_complete = false; // reached target this call
};

class DeviceTable {
 public:
  Device* find(const uint8_t addr[3]);
  const Device* find(const uint8_t addr[3]) const;

  // Find or insert a device for `addr`. Returns nullptr if the table is full. A new slot is
  // tagged with `source`; an existing slot keeps its (higher-priority) original source unless
  // `source` is more authoritative (CONFIGURED < DISCOVERED < OBSERVED, lower wins).
  Device* upsert(const uint8_t addr[3], Source source, uint32_t t_ms);

  // Mark a device seen now and flip it online. Sets *became_online if it was offline.
  void touch(Device* d, uint32_t t_ms, bool* became_online = nullptr);

  // Apply a parsed position report (native Somfy %). Updates movement/fault/stall.
  PosResult applyPosition(Device* d, const sdn::PositionReport& pr, uint32_t t_ms);

  // Apply parsed limits.
  bool applyLimits(Device* d, const sdn::LimitsReport& lr);

  // Begin a commanded move: record movement direction + target so applyPosition can detect
  // completion / stalls. target is native Somfy %.
  void beginMove(Device* d, sdn::MovementState dir, uint8_t target_pct);

  // Sweep for comms-loss. Any online device not seen within offline_ms flips offline; the
  // index of each transitioned device is reported via the callback-free out-array. Returns
  // the count of devices that went offline this sweep (their slots can be read via at()).
  size_t sweepOffline(uint32_t t_ms, uint32_t offline_ms, Device** out_offline, size_t max_out);

  // Remove a device from the table (e.g. a swapped-out / stale motor). Returns true if found.
  bool remove(const uint8_t addr[3]);

  size_t count() const;
  Device* at(size_t i);              // i over used slots, in insertion order
  const Device* at(size_t i) const;
  size_t capacity() const { return MAX_DEVICES; }

  void clear();

 private:
  Device devices_[MAX_DEVICES];
};

}  // namespace devices
