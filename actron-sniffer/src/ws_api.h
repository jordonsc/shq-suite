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

// Maximum seconds we hold a `*_transitioning` value waiting for the board to publish the
// requested change. After this we drop the transition value and continue reporting the
// actual board state — per the spec's GRACE_PERIOD convention.
constexpr uint32_t GRACE_PERIOD_MS = 60000;

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

}  // namespace ws_api
