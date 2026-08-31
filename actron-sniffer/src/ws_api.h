// Controller-facing WebSockets API on top of the MITM bridge.
//
// Spec: ACTRON-MITM-API.md. Designed for Home Assistant integration on a trusted LAN
// (no auth, no TLS). Bridge state (mode/fan/setpoints/zones) is pushed to all clients
// on change plus a periodic heartbeat. Client commands map to bridge inject/pulse rules
// using the recipes in LOCAL-CONTROL-RECIPES.md. Pending writes appear as
// `<field>_transitioning` values that replace optimistic updates on the client side.

#pragma once

#include <cstddef>
#include <cstdint>

#include "bridge.h"
#include "state.h"

namespace ws_api {

// Zone-setpoint give-up deadline, applied PER COMMIT PHASE. Zone setpoints are persistent
// INJECT rules held on every NEO response frame until the board adopts them — the continuous
// hold IS the retry, so there's no re-fire loop here; we just wait one grace window per phase
// before giving up. In AUTO the two phases (cool then heat) each get a fresh window, so an
// AUTO zone setpoint can legitimately take up to 2x this before bailing.
constexpr uint32_t ZONE_SETPOINT_GRACE_MS = 60000;

// Pulse commands (mode / fan / master setpoint / zone enable) are momentary 2-frame edges —
// if the pulse is missed the bus is left clean, so the remedy is to re-fire. When a pulse
// lands the board re-broadcasts within ~3-6 s; a transition still unadopted after the retry
// interval means that attempt was dropped. We re-fire every PULSE_RETRY_INTERVAL_MS up to
// PULSE_MAX_FIRES total attempts (initial fire + retries), then give up. 6 fires x 10 s gives
// an effective 60 s give-up window with far better odds than a single shot.
constexpr uint32_t PULSE_RETRY_INTERVAL_MS = 10000;
constexpr uint8_t PULSE_MAX_FIRES = 6;

// Heartbeat interval — clients can use this to detect a dead connection.
constexpr uint32_t HEARTBEAT_INTERVAL_MS = 10000;

// Hooks main.cpp passes in so ws_api can issue writes without depending on main's globals
// by name. `injector` is the NEO→board endpoint's StreamingBridge (g_b.bridge). `set_mode`
// is the function that flips the global bridge mode (and both endpoints' bridge instances).
struct Hooks {
  bridge::StreamingBridge* injector;
  bridge::StreamingBridge::Mode* mode_var;
  void (*set_mode)(bridge::StreamingBridge::Mode m);
};

// One-time init at boot, after WiFi is up. Starts the WS server on `port`.
void begin(uint16_t port, const Hooks& hooks);

// Drive from Arduino loop(): runs the WS server pump, ticks pending transitions against
// the latest state, and emits heartbeats.
void loop();

// Called from the frame-decode hook when state may have changed — safe to call from the
// priority-5 bridge task (its only entry point into this module). Cheap and socket-free:
// copies a snapshot under a spinlock and sets a dirty flag; ws_api::loop() (main loop) diffs
// against what was last published and broadcasts. The bridge task must never touch the WS
// layer directly: arduinoWebSockets does blocking socket I/O (up to WEBSOCKETS_TCP_TIMEOUT
// per client) with no internal locking, so a direct broadcast from the bridge task both
// races the main loop's server.loop() and stalls the RS485 relay on a zombie client
// (ledger shq-suite-0038). Ported from somfy-sdn's dirty-flag pattern.
void notifyStateChanged(const state::ControllerState& s);

// Stats — exposed so /stats can include client/transition counts.
size_t connectedClients();
size_t pendingTransitions();

// Lifetime WS event counters (since boot) — exposed so /stats can show connection churn.
// Diagnosing the 2026-08 silent-socket flaps (ledger shq-suite-0019): a healthy device has
// ws_conn ≈ ws_disc ≈ a handful; a large gap between them means sockets are being abandoned
// without a DISCONNECTED event (lwIP-layer death the WS library never saw).
uint32_t connectEvents();
uint32_t disconnectEvents();
uint32_t errorEvents();

// Heartbeat health (ledger shq-suite-0034). `hb_age` is the age of the last state push: it should
// never exceed HEARTBEAT_INTERVAL_MS by more than a loop or two. A large or growing value means
// the push has stalled — which is what HA sees as a device that connects but never sends. Check
// this BEFORE suspecting WiFi, lwIP or the WS library.
// Writes the write-guard declined because the socket would have blocked (fw 1.8.0).
uint32_t skippedWrites();

// Reaps declined because the client was still settling (fw 1.9.0). Climbing = the min-age gate
// is doing real work; a young socket went unwritable and we correctly did NOT kill it.
uint32_t deferredReaps();

uint32_t heartbeatBroadcasts();
uint32_t heartbeatAgeMs();

}  // namespace ws_api
