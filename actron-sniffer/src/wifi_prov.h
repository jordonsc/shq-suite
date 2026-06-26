// WiFi provisioning for the Actron MITM controller — ported from somfy-sdn/src/wifi_prov.{h,cpp}
// (trimmed: no motor/bus logic). NO baked-in credentials: SSID/password live in NVS (Arduino
// Preferences). Boot flow: read NVS -> STA connect with retries -> on absence/failure start a
// SoftAP captive portal (actron-mitm-XXXX) serving a WiFi-setup form. This replaces the old
// baked secrets.h creds, which knocked the in-wall device off the network when an image was
// built without secrets.h (ledger shq-suite-0013) and left no remote recovery path.
//
// The RS485 MITM bridge runs on its own FreeRTOS task and is UNAFFECTED by provisioning state —
// the A/C keeps bridging whether the controller is CONNECTED or sitting in the portal.

#pragma once

#include <cstdint>

namespace wifi_prov {

enum class Status : uint8_t { CONNECTED, PORTAL };

// Read NVS and either connect (STA) or start the SoftAP captive portal. Returns the resulting
// mode. In PORTAL mode the app HTTP/WS servers must NOT start (the portal owns port 80); the
// caller keeps the bridge task running and spins loop() until the device reboots on save.
Status begin();

// Drive from the Arduino loop: services the captive-portal HTTP+DNS (PORTAL mode) and the
// GPIO0 button + on-demand reconnect (both modes).
void loop();

Status status();
bool isConnected();
const char* hostname();  // "actron-mitm-XXXX"

// Wipe WiFi creds and reboot into the SoftAP portal. Triggered by HTTP POST /wifireset (there is
// no GPIO0 button here — GPIO0 is the NEO UART RX).
void factoryWipe();

// Request a manual WiFi reconnect (re-scan all channels, reassociate to the strongest AP).
// Deferred to loop() so a calling HTTP ack flushes before the link drops.
void requestReconnectBestAp();

}  // namespace wifi_prov
