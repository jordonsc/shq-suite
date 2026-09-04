// WiFi provisioning (SPEC §7) — replaces Matter commissioning. No baked-in credentials:
// SSID/password live in NVS (Arduino Preferences). Boot flow: read NVS -> STA connect with
// retries -> on absence/failure start a SoftAP captive portal (somfy-sdn-XXXX) serving a form.
// The GPIO0 momentary button (already wired on field devices): long-press wipes creds and
// reboots into the portal; short-press WINKs all detected motors.

#pragma once

#include <cstdint>

namespace wifi_prov {

enum class Status : uint8_t { CONNECTED, PORTAL };

using WinkCb = void (*)();

// Read NVS and either connect (STA) or start the SoftAP captive portal. Returns the resulting
// mode. In PORTAL mode the caller should just spin loop() until the device reboots.
Status begin();

// Drive from the Arduino loop: services the captive-portal HTTP+DNS (PORTAL mode) and the
// GPIO0 button (both modes).
void loop();

Status status();
bool isConnected();
const char* hostname();  // "somfy-sdn-XXXX"

// Persist credentials to NVS and reboot into STA (the authenticated POST /wifi path; auth is
// deferred per SPEC §7 — LAN-only). Returns false only if NVS write fails.
bool saveCredentials(const char* ssid, const char* password);

// Wipe WiFi creds (+ configured motors) and reboot into the portal.
void factoryWipe();

// Request a manual WiFi reconnect: drops the STA link and re-scans all channels to reassociate to
// the strongest AP for the SSID. Deferred to loop() so the calling HTTP/WS ack flushes first.
// Use after an AP that was offline at boot comes back, to move off a distant fallback AP. The
// Arduino stack has no live roaming, so this on-demand trigger is how a controller re-evaluates.
void requestReconnectBestAp();

// Record a reboot reason in NVS and restart. The note is read back (and cleared) on the next
// boot, surfaced as `note=` in /stats — so a fleet health sweep can tell a watchdog self-heal
// from a power cycle (which reads note=none reset=poweron). Never returns.
[[noreturn]] void noteReboot(const char* reason);

// The reboot note recorded by the PREVIOUS boot's noteReboot(), or "none". Valid after begin().
const char* bootNote();

// WiFi link-churn telemetry (fw 1.5.0) — lifetime STA disconnect events since boot and the last
// 802.11 disconnect reason code. Surfaced in /stats as wifi_disc=/wifi_reason= so a fleet sweep
// can see link instability (e.g. after network infra work) before it escalates to a watchdog
// reboot.
uint32_t staDisconnectCount();
uint8_t lastDisconnectReason();

// Network-stack watchdog (fw 1.11.0, ledger shq-suite-0044; policy in netwatch.h). The link
// watchdog above only sees a DOWN link; Bed 2 died with the link UP — associated, transmitting,
// answering nothing. This one probes the gateway every NET_PROBE_INTERVAL_MS, re-associates on a
// large backward clock step / sustained unreachability / sustained low heap, and reboots only if
// a re-association did not help. Call noteInbound() on every inbound WS frame or pong: recent
// inbound traffic is proof the stack can receive and vetoes the "unreachable" trigger, which is
// what stops an HA outage (or a gateway that drops ICMP) from ever tripping it.
void noteInbound();
uint32_t netProbeFailures();   // consecutive gateway probes unanswered (0 when healthy)
uint32_t netRecoveries();      // watchdog-driven re-associations since boot
uint32_t netProbes();          // gateway probes sent since boot
const char* netLastReason();   // reason of the newest watchdog action, "none" until one fires

// Load the configured motor addresses ("AA:BB:CC,...") from NVS and register them with the
// bus device table (CONFIGURED source). Called by main after bus::begin().
void loadConfiguredMotors();

// Short-press of GPIO0 fires this (wink all motors).
void setWinkCallback(WinkCb cb);

}  // namespace wifi_prov
