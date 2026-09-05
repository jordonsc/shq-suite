// WiFi protocol A/B knob: which 802.11 generations the station may negotiate.
//
// Twin of actron-sniffer/src/wifi_proto.h — keep the two in step (same rule as mono/fault/diag).
//
// WHY THIS EXISTS (fw 1.13.0, 2026-09-05, ledger shq-suite-0046). Two of twelve controllers on one
// AP radio lose LONG uplink frames (>= ~700 B) on consecutive retransmissions while every short
// frame in the same burst arrives, and a packet capture put that loss in the air, not in either
// TCP stack. The ranked physical suspects are all on the 11ax (HE20) path these boards negotiate
// against the UniFi U6 Enterprise radio. The cheapest falsifiable experiment is to take one
// churning unit off 11ax (HE -> HT, `bgn`) and, if that is not enough, off 11n (`bg`, no A-MPDU,
// no MCS), with its clean neighbour as the control — without a reflash, so the fleet can run one
// image and individual units can be switched from HA or curl.
//
// The knob is persisted in NVS (Preferences key wifi_proto::NVS_KEY in the firmware's namespace)
// and applied with esp_wifi_set_protocol() immediately before every WiFi.begin(). Every value —
// `bgnax` included — is written EXPLICITLY (fw 1.14.1): 1.13.0 made no call for the default so
// the path would stay byte-for-byte pre-1.13.0, but the IDF driver persists the last bitmap in
// its own NVS namespace (CONFIG_ESP_WIFI_NVS_ENABLED), so a unit once switched to `bgn` stayed
// HT20 through every later "bgnax" reboot. The driver's own post-init default is B|G|N|AX on the
// C6 (esp_wifi.h); the Arduino core only rewrites the bitmap for long-range mode, never used here.
//
// Pure C++: no Arduino dependency, host-tested (test/test_wifi_proto). The driver glue lives in
// wifi_prov.cpp.

#pragma once

#include <cstdint>
#include <cstring>

namespace wifi_proto {

enum class Proto : uint8_t {
  BGNAX = 0,  // 802.11b/g/n/ax (HE20 on the C6) — set EXPLICITLY: the driver persists the last
              // bitmap in its own NVS, so "no call" would inherit whatever was set before (0046).
  BGN = 1,    // 802.11b/g/n — HT20, no HE
  BG = 2,     // 802.11b/g — legacy OFDM/DSSS only, no A-MPDU, no MCS
};

constexpr const char* NVS_KEY = "wifi_proto";
constexpr uint8_t COUNT = 3;

// WIFI_PROTOCOL_11B / 11G / 11N from esp_wifi_types_generic.h, spelled out so this header stays
// Arduino-free. wifi_prov.cpp static_asserts them against the IDF macros.
constexpr uint8_t BIT_11B = 0x1;
constexpr uint8_t BIT_11G = 0x2;
constexpr uint8_t BIT_11N = 0x4;
constexpr uint8_t BIT_11AX = 0x40;  // WIFI_PROTOCOL_11AX

// The slug used everywhere: NVS, /stats `proto=`, the health push, the WS command, HA's select.
constexpr const char* name(Proto p) {
  switch (p) {
    case Proto::BGN: return "bgn";
    case Proto::BG: return "bg";
    case Proto::BGNAX: break;
  }
  return "bgnax";
}

// Driver bitmap for esp_wifi_set_protocol(). Never 0: the default is written explicitly (1.14.1).
constexpr uint8_t bitmap(Proto p) {
  switch (p) {
    case Proto::BGN: return BIT_11B | BIT_11G | BIT_11N;
    case Proto::BG: return BIT_11B | BIT_11G;
    case Proto::BGNAX: break;
  }
  return BIT_11B | BIT_11G | BIT_11N | BIT_11AX;
}

// Exact, case-sensitive slug match. Anything else (including "" and null) is rejected and *out
// is left untouched, so a corrupt or absent NVS value falls through to the caller's default.
inline bool parse(const char* s, Proto* out) {
  if (s == nullptr || out == nullptr) return false;
  for (uint8_t i = 0; i < COUNT; i++) {
    const Proto p = static_cast<Proto>(i);
    if (strcmp(s, name(p)) == 0) {
      *out = p;
      return true;
    }
  }
  return false;
}

}  // namespace wifi_proto
