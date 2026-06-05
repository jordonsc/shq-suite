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

// Load the configured motor addresses ("AA:BB:CC,...") from NVS and register them with the
// bus device table (CONFIGURED source). Called by main after bus::begin().
void loadConfiguredMotors();

// Short-press of GPIO0 fires this (wink all motors).
void setWinkCallback(WinkCb cb);

}  // namespace wifi_prov
