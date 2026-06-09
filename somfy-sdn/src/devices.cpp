#include "devices.h"

#include <cstdio>
#include <cstring>

namespace devices {

Device* DeviceTable::find(const uint8_t addr[3]) {
  for (size_t i = 0; i < MAX_DEVICES; i++) {
    if (devices_[i].used && sdn::addrEqual(devices_[i].addr, addr)) return &devices_[i];
  }
  return nullptr;
}

const Device* DeviceTable::find(const uint8_t addr[3]) const {
  for (size_t i = 0; i < MAX_DEVICES; i++) {
    if (devices_[i].used && sdn::addrEqual(devices_[i].addr, addr)) return &devices_[i];
  }
  return nullptr;
}

Device* DeviceTable::upsert(const uint8_t addr[3], Source source, uint32_t t_ms) {
  Device* d = find(addr);
  if (d != nullptr) {
    // Keep the most authoritative known source (lower enum value wins).
    if ((uint8_t)source < (uint8_t)d->source) d->source = source;
    return d;
  }
  for (size_t i = 0; i < MAX_DEVICES; i++) {
    if (!devices_[i].used) {
      Device& nd = devices_[i];
      nd = Device{};  // reset to defaults
      nd.used = true;
      memcpy(nd.addr, addr, 3);
      nd.source = source;
      nd.last_seen_ms = t_ms;
      return &nd;
    }
  }
  return nullptr;  // table full
}

void DeviceTable::touch(Device* d, uint32_t t_ms, bool* became_online) {
  if (d == nullptr) return;
  bool was_online = d->online;
  d->last_seen_ms = t_ms;
  d->online = true;
  if (became_online != nullptr) *became_online = (!was_online);
}

PosResult DeviceTable::applyPosition(Device* d, const sdn::PositionReport& pr, uint32_t t_ms) {
  PosResult res;
  if (d == nullptr) return res;
  d->last_seen_ms = t_ms;

  if (pr.fault || !pr.valid) {
    // Always drop movement here: a commanded move that the motor can't satisfy (e.g. no limits)
    // would otherwise leave `moving` stuck forever, since faulted reads never reach a target or
    // trip the stall counter.
    d->movement = sdn::MovementState::IDLE;
    d->stall_count = 0;
    if (!d->fault) {
      d->fault = true;
      res.became_fault = true;
      res.changed = true;
    }
    snprintf(d->status, sizeof(d->status), "position unknown");
    return res;
  }

  // Valid reading — clear any prior fault + status.
  if (d->fault) {
    d->fault = false;
    res.cleared_fault = true;
    res.changed = true;
  }
  d->status[0] = '\0';

  uint8_t old_pct = d->position_pct;
  uint16_t old_pulses = d->position_pulses;
  bool first = !d->position_known;
  d->position_known = true;
  d->position_pulses = pr.pulses;

  // Reject a spurious percent jump. The encoder pulse count is the ground truth for "did it move";
  // when the motor is idle and pulses are unchanged the blind physically cannot have moved, so a
  // changed percent byte is a glitch in POST_MOTOR_POSITION (observed in the field: percent
  // momentarily reads 100 while pulses stay put, flapping the HA cover closed→open). Keep the last
  // percent and don't broadcast a phantom change. Only suppresses when pulses match exactly, so a
  // real move (which always moves the encoder) is never affected.
  bool spurious = !first && d->movement == sdn::MovementState::IDLE && pr.pulses == old_pulses &&
                  pr.percent != old_pct;

  if ((old_pct != pr.percent && !spurious) || first) {
    d->position_pct = pr.percent;
    d->stall_count = 0;
    res.changed = true;
  }

  // Movement completion: reached the commanded target.
  if (d->movement != sdn::MovementState::IDLE && pr.percent == d->target_pct) {
    d->movement = sdn::MovementState::IDLE;
    d->stall_count = 0;
    res.movement_complete = true;
    res.changed = true;
  }

  // Stall detection: position unchanged while moving.
  if (d->movement != sdn::MovementState::IDLE && old_pct == pr.percent && !first) {
    d->stall_count++;
    if (d->stall_count >= STALL_THRESHOLD) {
      d->fault = true;
      d->movement = sdn::MovementState::IDLE;
      d->stall_count = 0;
      snprintf(d->status, sizeof(d->status), "stalled");
      res.stalled = true;
      res.became_fault = true;
      res.changed = true;
    }
  }

  return res;
}

bool DeviceTable::applyLimits(Device* d, const sdn::LimitsReport& lr) {
  if (d == nullptr || !lr.valid) return false;
  // 0xFFFF on a limit means "not set" (factory/after a reset). limits_known reflects whether at
  // least one limit is actually programmed, so a reset clears it (and HA shows the limit as
  // unknown). While unset, the poll loop keeps re-reading until a limit is programmed.
  bool any_set = (lr.up_pulses != sdn::POSITION_UNKNOWN) || (lr.down_pulses != sdn::POSITION_UNKNOWN);
  bool changed = (d->limits_known != any_set) || d->up_limit_pulses != lr.up_pulses ||
                 d->down_limit_pulses != lr.down_pulses;
  d->up_limit_pulses = lr.up_pulses;
  d->down_limit_pulses = lr.down_pulses;
  d->limits_known = any_set;
  return changed;
}

void DeviceTable::beginMove(Device* d, sdn::MovementState dir, uint8_t target_pct) {
  if (d == nullptr) return;
  d->movement = dir;
  d->target_pct = target_pct;
  d->stall_count = 0;
}

size_t DeviceTable::sweepOffline(uint32_t t_ms, uint32_t offline_ms, Device** out_offline,
                                 size_t max_out) {
  size_t n = 0;
  for (size_t i = 0; i < MAX_DEVICES; i++) {
    Device& d = devices_[i];
    if (!d.used || !d.online) continue;
    if ((uint32_t)(t_ms - d.last_seen_ms) >= offline_ms) {
      d.online = false;
      if (out_offline != nullptr && n < max_out) out_offline[n] = &d;
      n++;
    }
  }
  return n;
}

bool DeviceTable::remove(const uint8_t addr[3]) {
  for (size_t i = 0; i < MAX_DEVICES; i++) {
    if (devices_[i].used && sdn::addrEqual(devices_[i].addr, addr)) {
      devices_[i] = Device{};
      return true;
    }
  }
  return false;
}

size_t DeviceTable::count() const {
  size_t n = 0;
  for (size_t i = 0; i < MAX_DEVICES; i++) if (devices_[i].used) n++;
  return n;
}

Device* DeviceTable::at(size_t i) {
  size_t seen = 0;
  for (size_t k = 0; k < MAX_DEVICES; k++) {
    if (devices_[k].used) {
      if (seen == i) return &devices_[k];
      seen++;
    }
  }
  return nullptr;
}

const Device* DeviceTable::at(size_t i) const {
  size_t seen = 0;
  for (size_t k = 0; k < MAX_DEVICES; k++) {
    if (devices_[k].used) {
      if (seen == i) return &devices_[k];
      seen++;
    }
  }
  return nullptr;
}

void DeviceTable::clear() {
  for (size_t i = 0; i < MAX_DEVICES; i++) devices_[i] = Device{};
}

}  // namespace devices
