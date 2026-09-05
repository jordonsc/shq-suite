#include "txstats.h"

#include <cstdio>
#include <cstring>

namespace txstats {

void Counters::add(const Counters& o) {
  tx_enable += o.tx_enable;
  tx_complete += o.tx_complete;
  tx_succ += o.tx_succ;
  retry_edca += o.retry_edca;
  retry_tb += o.retry_tb;
  tb_times += o.tb_times;
  rx_ack += o.rx_ack;
  rx_ba += o.rx_ba;
  timeout += o.timeout;
  collision += o.collision;
  tx_no_mem += o.tx_no_mem;
  tx_error_a0 += o.tx_error_a0;
  fail_count += o.fail_count;
  fail_timeout += o.fail_timeout;
  if (o.seq_max_rtt_us > seq_max_rtt_us) seq_max_rtt_us = o.seq_max_rtt_us;
}

Counters Counters::since(const Counters& b) const {
  Counters d;
  d.tx_enable = deltaFrom(b.tx_enable, tx_enable);
  d.tx_complete = deltaFrom(b.tx_complete, tx_complete);
  d.tx_succ = deltaFrom(b.tx_succ, tx_succ);
  d.retry_edca = deltaFrom(b.retry_edca, retry_edca);
  d.retry_tb = deltaFrom(b.retry_tb, retry_tb);
  d.tb_times = deltaFrom(b.tb_times, tb_times);
  d.rx_ack = deltaFrom(b.rx_ack, rx_ack);
  d.rx_ba = deltaFrom(b.rx_ba, rx_ba);
  d.timeout = deltaFrom(b.timeout, timeout);
  d.collision = deltaFrom(b.collision, collision);
  d.tx_no_mem = deltaFrom(b.tx_no_mem, tx_no_mem);
  d.tx_error_a0 = deltaFrom(b.tx_error_a0, tx_error_a0);
  d.fail_count = deltaFrom(b.fail_count, fail_count);
  d.fail_timeout = deltaFrom(b.fail_timeout, fail_timeout);
  d.seq_max_rtt_us = seq_max_rtt_us;
  return d;
}

}  // namespace txstats

#ifdef ARDUINO

#include <Arduino.h>
#include <esp_wifi.h>
#include <esp_wifi_he.h>
// PRIVATE header: declares esp_wifi_get_tx_statistics()/esp_wifi_clr_tx_statistics() and the
// esp_test_tx_*_statistics_t layouts. Both functions are exported (T) by libnet80211.a in the
// prebuilt Arduino libs, and the struct layouts come from the same header the library was
// built against, so this is stable for a given framework version — but re-check the layouts
// (esp_wifi_he_types_private.h) on any pioarduino platform bump.
#include <esp_private/esp_wifi_he_private.h>

namespace txstats {

namespace {

// The driver clears on request, not on read (its own example clears explicitly), so every
// reading here is followed by a clear and is therefore a delta. If a clear ever fails the next
// reading is cumulative again; last_raw_ + deltaFrom() covers that case.
constexpr uint32_t PHY_REFRESH_MS = 60 * 1000;

Accumulator g_acc;
uint8_t g_enabled = 0;
uint32_t g_samples = 0;
bool g_clear_ok[ESP_WIFI_ACI_MAX] = {true, true, true, true};
Counters g_last_raw[ESP_WIFI_ACI_MAX];
bool g_link_was_up = false;
uint32_t g_last_phy_ms = 0;
char g_phy[48] = "-";
uint8_t g_channel = 0;

// Driver contract (libpp test_hal_tx_statis.o, IDF 5.5.4, ledger shq-suite-0047): `tx_fail` is an
// ARRAY of TEST_TX_FAIL_MAX structs, one per esp_test_tx_fail_state_t — the getter memcpy()s
// TEST_TX_FAIL_MAX * sizeof(esp_test_tx_fail_statistics_t) = 984 bytes into it. The public
// prototype declares a bare pointer and says nothing about that; passing a single struct (fw
// 1.12.0) overran the caller's stack frame by 820 bytes and put the Bed 2 canary in a reboot
// loop on the first loop() pass. The static_asserts are tripwires for a platform bump.
static_assert(sizeof(esp_test_tx_statistics_t) == 136, "driver copies 136 B into tx_stats");
static_assert(sizeof(esp_test_tx_fail_statistics_t) == 164, "driver copies 6 x 164 B into tx_fail");

Counters fromDriver(const esp_test_tx_statistics_t& s,
                    const esp_test_tx_fail_statistics_t (&f)[TEST_TX_FAIL_MAX]) {
  Counters c;
  c.tx_enable = s.tx_enable;
  c.tx_complete = s.tx_complete;
  c.tx_succ = s.tx_succ;
  c.retry_edca = s.retry_edca;
  c.retry_tb = s.retry_tb;
  c.tb_times = s.tb_times;
  c.rx_ack = s.rx_ack;
  c.rx_ba = s.rx_ba;
  c.timeout = s.timeout;
  c.collision = s.collision;
  c.tx_no_mem = s.tx_no_mem;
  c.tx_error_a0 = s.tx_error_a0;
  // State 0 is TEST_TX_SUCCESS; the failure states are 1..TEST_TX_FAIL_MAX-1.
  uint32_t fails = 0, to = 0;
  for (int k = TEST_TX_FAIL_RTS; k < TEST_TX_FAIL_MAX; k++) {
    fails += f[k].count;
    for (int e = 0; e < TEST_TX_FAIL_ERROR_MAX; e++) to += f[k].match[TEST_TX_WAIT_TIMEOUT][e];
  }
  c.fail_count = fails;
  c.fail_timeout = to;
  c.seq_max_rtt_us = s.tx_seq_max_rtt;
  return c;
}

void enableAll() {
  // Enable only what the driver says is not yet enabled: each enable allocates in the WiFi task
  // and this runs on every tick where the bitmap is short (re-association may reset it).
  const uint8_t have = esp_wifi_get_tx_statistics_ena_acibitmap();
  for (int a = 0; a < ESP_WIFI_ACI_MAX; a++) {
    if (have & (1u << a)) continue;
    esp_wifi_enable_tx_statistics((esp_wifi_aci_t)a, true);
  }
  g_enabled = esp_wifi_get_tx_statistics_ena_acibitmap();
}

void readAll() {
  for (int a = 0; a < ESP_WIFI_ACI_MAX; a++) {
    if (!(g_enabled & (1u << a))) continue;
    // static: 1.1 kB is too much to put on loopTask's 8 kB stack once a second, and the fail
    // block MUST be the full per-state array (see fromDriver) — the driver writes all of it.
    static esp_test_tx_statistics_t s;
    static esp_test_tx_fail_statistics_t f[TEST_TX_FAIL_MAX];
    memset(&s, 0, sizeof(s));
    memset(f, 0, sizeof(f));
    if (esp_wifi_get_tx_statistics((esp_wifi_aci_t)a, &s, f) != ESP_OK) continue;
    const Counters raw = fromDriver(s, f);
    Counters d = raw;
    if (!g_clear_ok[a]) d = raw.since(g_last_raw[a]);  // previous clear failed: still cumulative
    g_acc.add(d);
    g_last_raw[a] = raw;
    g_clear_ok[a] = (esp_wifi_clr_tx_statistics((esp_wifi_aci_t)a) == ESP_OK);
    if (g_clear_ok[a]) g_last_raw[a] = Counters{};
    g_samples++;
  }
}

const char* phyModeName(wifi_phy_mode_t m) {
  switch (m) {
    case WIFI_PHY_MODE_LR: return "LR";
    case WIFI_PHY_MODE_11B: return "11B";
    case WIFI_PHY_MODE_11G: return "11G";
    case WIFI_PHY_MODE_11A: return "11A";
    case WIFI_PHY_MODE_HT20: return "HT20";
    case WIFI_PHY_MODE_HT40: return "HT40";
    case WIFI_PHY_MODE_HE20: return "HE20";
    case WIFI_PHY_MODE_VHT20: return "VHT20";
  }
  return "?";
}

void refreshPhy() {
  wifi_phy_mode_t mode = WIFI_PHY_MODE_11B;
  const bool have_mode = (esp_wifi_sta_get_negotiated_phymode(&mode) == ESP_OK);
  wifi_ap_record_t ap;
  memset(&ap, 0, sizeof(ap));
  const bool have_ap = (esp_wifi_sta_get_ap_info(&ap) == ESP_OK);
  if (!have_mode && !have_ap) {
    strcpy(g_phy, "-");
    g_channel = 0;
    return;
  }
  char flags[12] = {0};
  size_t n = 0;
  if (ap.phy_11b) flags[n++] = 'b';
  if (ap.phy_11g) flags[n++] = 'g';
  if (ap.phy_11n) flags[n++] = 'n';
  if (ap.phy_11ax) { flags[n++] = 'a'; flags[n++] = 'x'; }
  if (ap.phy_lr) { flags[n++] = 'L'; flags[n++] = 'R'; }
  if (n == 0) flags[n++] = '-';
  g_channel = ap.primary;
  const unsigned bw = (ap.bandwidth == WIFI_BW_HT40) ? 40 : 20;
  snprintf(g_phy, sizeof(g_phy), "%s ch%u bw%u %s%s%s%s", have_mode ? phyModeName(mode) : "?",
           (unsigned)ap.primary, bw, flags, ap.wps ? " wps" : "",
           ap.ftm_responder ? " ftm" : "", ap.he_ap.bss_color ? " color" : "");
}

}  // namespace

void tick(uint32_t now_ms, bool link_up) {
  if (!link_up) {
    g_link_was_up = false;
    return;
  }
  const bool came_up = !g_link_was_up;
  g_link_was_up = true;
  // Enable on link-up and re-check on the PHY refresh cadence (not every tick): a driver that
  // refuses one AC would otherwise be asked again once a second for ever.
  const bool refresh =
      came_up || g_last_phy_ms == 0 || (uint32_t)(now_ms - g_last_phy_ms) >= PHY_REFRESH_MS;
  if (refresh) {
    g_last_phy_ms = now_ms;
    if (g_enabled != ((1u << ESP_WIFI_ACI_MAX) - 1)) enableAll();
    refreshPhy();
  }
  readAll();
}

const Accumulator& acc() { return g_acc; }
Accumulator& accMutable() { return g_acc; }
uint8_t enabledAcis() { return g_enabled; }
uint32_t samples() { return g_samples; }
const char* phyString() { return g_phy; }
uint8_t channel() { return g_channel; }

}  // namespace txstats

#endif  // ARDUINO
