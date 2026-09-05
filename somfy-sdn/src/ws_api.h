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

// FRAME SIZE RULE (fw 1.14.0, ledger shq-suite-0046). No frame this module sends on the hot path
// may exceed ws_liveness::FRAME_BUDGET_BYTES (600). On two boards uplink WiFi frame loss rises
// steeply with length — measured on the wire: 0 of 313 ~330 B frames lost, 8 of 65 ~720 B, 2 of
// 4 >= 1300 B — and one lost segment head-of-line-blocks the whole TCP queue for the length of
// lwIP's retransmit ladder. So: the diag backlog is a record per frame (never one frame), the
// health push is three frames a second apart (`health` / `health_ws` / `health_net`), telemetry
// is held back while the pcb is retransmitting, and the guard counts any oversize frame as
// `big_frames` in `health_ws` — that counter must read 0. Measure with serializeJson before
// adding keys to any push. The keepalive is ours, not the library's: see ws_liveness.h.

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
// Writes the write-guard declined because the socket would have blocked (fw 1.8.0).
uint32_t skippedWrites();

// Reaps declined because the client was still settling (fw 1.9.0). Climbing = the min-age gate
// is doing real work; a young socket went unwritable and we correctly did NOT kill it.
uint32_t deferredReaps();

uint32_t heartbeatBroadcasts();
uint32_t heartbeatAgeMs();

}  // namespace ws_api
