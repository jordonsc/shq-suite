// WiFi MAC-layer transmit telemetry: are our frames being retried to exhaustion on the air?
//
// Twin of actron-sniffer/src/txstats.{h,cpp} — keep the two in step (same rule as mono/diag).
//
// WHY THIS EXISTS (fw 1.12.0, 2026-09-05). Two of twelve controllers on one AP radio churn their
// HA WebSocket ~25-50 times a day while the other ten, same firmware, same radio, do not. A
// packet capture put the loss in the UPLINK and made it a function of frame LENGTH: 731 B and
// 1436 B TCP segments were lost four retransmissions in a row while 2-332 B frames sent in the
// same burst all arrived, and the 64 B gateway ICMP never once failed. Everything above the MAC —
// lwIP, the socket pool, the write-guard, the clock — has been measured clean. What has never been
// measured is the MAC itself: whether the WiFi driver is retrying those long frames until it
// gives up, and whether it is being told so by the AP (no ACK / no Block-ACK) or losing them
// before that (CTS timeout, collision, TB-PPDU failures on the 11ax path).
//
// The ESP32-C6 driver keeps exactly those counters per access category, behind
// `esp_wifi_enable_tx_statistics()` / `esp_wifi_get_tx_statistics()`. The getter is a public
// symbol of libnet80211 but its prototype lives in a PRIVATE header
// (esp_private/esp_wifi_he_private.h) and the sdkconfig option that would enable it at init
// (CONFIG_ESP_WIFI_ENABLE_WIFI_TX_STATS) is OFF in the prebuilt Arduino libs — so it is enabled
// here at runtime, per AC, after the station associates. Reads go through the same ioctl path
// as WiFi.RSSI(), which diag already calls at 1 Hz.
//
// STRICTLY READ-ONLY. Nothing here changes a rate, a PHY mode, a timeout or a reconnect. It is the
// baseline half of an A/B: flash it, watch `tx_retry`/`tx_to` against the HA churn, then decide.
//
// The pure accumulator (Counters/Accumulator) has no Arduino dependency and is host-tested; the
// driver glue lives in txstats.cpp behind the ARDUINO guard.

#pragma once

#include <cstdint>

namespace txstats {

// One reading of the driver's per-AC transmit counters, or a sum of readings. Every field is a
// count except seq_max_rtt_us, which is a maximum (and raw — see below). Names follow esp_test_tx_statistics_t so the
// mapping back to Espressif's documentation is one-to-one.
struct Counters {
  uint32_t tx_enable = 0;      // frames handed to the hardware
  uint32_t tx_complete = 0;    // frames the hardware finished with (success or not)
  uint32_t tx_succ = 0;        // frames acknowledged
  uint32_t retry_edca = 0;     // retransmissions on the contention (EDCA) path
  uint32_t retry_tb = 0;       // retransmissions inside an 11ax trigger-based (TB) PPDU
  uint32_t tb_times = 0;       // transmissions that went out as a TB PPDU at all
  uint32_t rx_ack = 0;         // plain ACKs received for EDCA transmissions
  uint32_t rx_ba = 0;          // Block-ACKs received for EDCA transmissions
  uint32_t timeout = 0;        // ACK/BA never arrived
  uint32_t collision = 0;      // hardware-detected collisions
  uint32_t tx_no_mem = 0;      // driver could not allocate for a transmission
  uint32_t tx_error_a0 = 0;    // driver's own catch-all error counter
  uint32_t fail_count = 0;     // entries in the failure state matrix (esp_test_tx_fail_statistics_t)
  uint32_t fail_timeout = 0;   // ...of which the wait-state was TIMEOUT (no CTS / no ACK)
  // The driver's tx_seq_max_rtt, carried RAW and NOT surfaced anywhere since fw 1.13.0: a
  // 10-minute-old unit reported 545,967,505, so whatever its unit or origin, it is not the
  // microsecond RTT the name suggests. Kept in the accumulator (maximum semantics, host-tested)
  // so it can be re-exposed once its meaning is known.
  uint32_t seq_max_rtt_us = 0;

  void add(const Counters& o);
  // this - base, field by field, for a window. Maxima are carried, not subtracted.
  Counters since(const Counters& base) const;
  // Retries as a sum, the number most people want first.
  uint32_t retries() const { return retry_edca + retry_tb; }
};

// A counter that was cleared between two readings reads smaller than before. Treat that as a
// reset and take the new value whole rather than producing a wrapped 4-billion delta.
inline uint32_t deltaFrom(uint32_t prev, uint32_t cur) { return cur >= prev ? cur - prev : cur; }

// Lifetime totals plus two independent windows — "since the last health push" and "since the
// last diag record" — so both consumers can take a delta without disturbing each other.
class Accumulator {
 public:
  void add(const Counters& reading) { total_.add(reading); }
  const Counters& total() const { return total_; }

  Counters sinceHealth() const { return total_.since(health_base_); }
  void markHealth() { health_base_ = total_; }
  Counters sinceRecord() const { return total_.since(record_base_); }
  void markRecord() { record_base_ = total_; }

 private:
  Counters total_;
  Counters health_base_;
  Counters record_base_;
};

#ifdef ARDUINO

// Drive at ~1 Hz from the network tick with the STA link state. Enables the driver's statistics
// on every access category the first time the link is up (and again if the driver forgets them
// across a re-association), reads and clears them once a second, and refreshes the negotiated
// PHY description when the link comes up and every PHY_REFRESH_S after.
void tick(uint32_t now_ms, bool link_up);

const Accumulator& acc();
Accumulator& accMutable();  // for the two window marks

// Bitmap of access categories the driver confirms statistics are enabled on (bit = esp_wifi_aci_t).
// 0 means the enable never took — every counter above is then meaningless and reads zero.
uint8_t enabledAcis();
uint32_t samples();  // successful reads since boot; a flat value with the link up is a dead getter

// "HE20 ch6 bw20 bgnax" — negotiated PHY mode, primary channel, bandwidth, the AP's advertised
// PHY set (b/g/n/ax/lr flags) and wps/ftm if advertised. "-" until associated.
const char* phyString();
uint8_t channel();

#endif  // ARDUINO

}  // namespace txstats
