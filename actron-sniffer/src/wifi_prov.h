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

#include "wifi_proto.h"

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

// Network-stack watchdog (fw 1.11.0, ledger shq-suite-0044; policy in netwatch.h, twin of the
// somfy-sdn one). Probes the gateway every minute and re-associates on a large backward clock
// step, sustained unreachability with the link up, or sustained low heap. NO reboot tier on this
// device (the relay must never restart with the A/C running, shq-suite-0042). Call noteInbound()
// on every inbound WS frame or pong: recent inbound traffic vetoes the "unreachable" trigger, so
// an HA outage or a gateway that drops ICMP can never trip it on its own.
void noteInbound();
uint32_t netProbeFailures();   // consecutive gateway probes unanswered (0 when healthy)
uint32_t netRecoveries();      // watchdog-driven re-associations since boot
uint32_t netProbes();          // gateway probes sent since boot
const char* netLastReason();   // reason of the newest watchdog action, "none" until one fires

// WiFi protocol A/B knob (fw 1.13.0, ledger shq-suite-0046; the pure part is wifi_proto.h).
// Persisted in NVS (key wifi_proto::NVS_KEY in this firmware's namespace), read once in begin()
// and applied with esp_wifi_set_protocol() immediately before every WiFi.begin() — the boot
// connect, reassociate() (manual / netwatch) and, on the somfy twin, the link-retry loop.
// `bgnax` (the default) is written explicitly too (1.14.1). setWifiProto() persists + caches and
// returns false only if the NVS write failed; it does NOT touch the live link — call
// requestReconnectBestAp() afterwards so the new bitmap lands on a fresh association.
wifi_proto::Proto wifiProto();
bool setWifiProto(wifi_proto::Proto p);

// One-shot: esp_phy_erase_cal_data_in_nvs(). The NEXT boot then performs a full RF calibration
// instead of the partial one CONFIG_ESP_PHY_RF_CAL_PARTIAL does against the stored data. The
// caller reboots (noteReboot("phycal")); nothing here does. Returns false if the NVS erase failed.
bool erasePhyCalibration();

}  // namespace wifi_prov
