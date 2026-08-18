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

// Called from the frame-decode hook in main when state may have changed. We diff against
// what we last published; if anything tracked changed, push to all clients.
void publishStateIfChanged(const state::ControllerState& s);

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
uint32_t heartbeatBroadcasts();
uint32_t heartbeatAgeMs();

}  // namespace ws_api
