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

// Lifetime WS event counters (since boot) — exposed so /stats can show connection churn.
// Ported from actron-sniffer (ledger shq-suite-0019/0022 silent-socket diagnosis): a healthy
// device has ws_conn ≈ ws_disc ≈ a handful; a growing gap between them means sockets are being
// abandoned without a DISCONNECTED event (lwIP-layer death the WS library never saw).
uint32_t connectEvents();
uint32_t disconnectEvents();
uint32_t errorEvents();

// Heartbeat health (fw 1.5.1, ledger shq-suite-0034). `hb_age` in /stats is the age of the last
// state broadcast: it should never exceed HEARTBEAT_INTERVAL_MS by more than a loop or two. A
// large or growing value means the periodic push has stalled, which is what HA sees as a device
// that connects but never sends — check this BEFORE suspecting WiFi, lwIP or the WS library.
uint32_t heartbeatBroadcasts();
uint32_t heartbeatAgeMs();

}  // namespace ws_api
