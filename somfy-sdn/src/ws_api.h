// WebSocket Controller API on port 8767 (SPEC §9). Runtime surface for Home Assistant —
// push-based, same shape as the Actron MITM controller's WS API. On connect the server sends a
// full `state` snapshot, then a snapshot on every change plus a ~10 s heartbeat. Commands are
// JSON {type:"command", command:"...", addr:"AA:BB:CC", id:"..."}; the server replies with an
// `ack`/`error` correlated by `id`. Writes have no optimistic state — the firmware reports what
// motors confirm via polling (the device table), so state pushes reflect reality.
//
// State changes detected by the bus task set a dirty flag (notifyStateChanged); the actual
// broadcast happens from loop() on the Arduino main loop, so the WebSockets server is only ever
// driven from one context.

#pragma once

#include <cstdint>

namespace ws_api {

constexpr uint32_t HEARTBEAT_INTERVAL_MS = 10000;

void begin(uint16_t port);
void loop();

// Called from the bus task when the device table / mode changed — just flags dirty.
void notifyStateChanged();

uint32_t connectedClients();

}  // namespace ws_api
