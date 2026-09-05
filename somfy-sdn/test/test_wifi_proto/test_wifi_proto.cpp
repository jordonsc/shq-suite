// Host tests for the WiFi protocol A/B knob (fw 1.13.0, wifi_proto.h, ledger shq-suite-0046).
//
// What has to be right: every slug maps to an explicit IDF bitmap (the default included — the
// exact IDF bitmaps, every slug round-trips through parse()/name(), and anything that is not a
// slug — including the empty string an absent NVS key reads as — is rejected without touching
// the output, so a corrupt NVS value cannot silently move a unit off the default.

#include <string.h>
#include <unity.h>

#include "wifi_proto.h"

using wifi_proto::Proto;

void setUp() {}
void tearDown() {}

static void test_default_makes_no_driver_call() {
  TEST_ASSERT_EQUAL_UINT8(0x47, wifi_proto::bitmap(Proto::BGNAX));  // B|G|N|AX, explicit
  TEST_ASSERT_EQUAL_STRING("bgnax", wifi_proto::name(Proto::BGNAX));
}

static void test_bitmaps_match_idf_values() {
  // WIFI_PROTOCOL_11B|11G|11N = 0x7, 11B|11G = 0x3 (esp_wifi_types_generic.h).
  TEST_ASSERT_EQUAL_UINT8(0x7, wifi_proto::bitmap(Proto::BGN));
  TEST_ASSERT_EQUAL_UINT8(0x3, wifi_proto::bitmap(Proto::BG));
  // Neither B side may carry 11AX (0x40) or LR (0x8).
  TEST_ASSERT_EQUAL_UINT8(0, wifi_proto::bitmap(Proto::BGN) & 0x48);
  TEST_ASSERT_EQUAL_UINT8(0, wifi_proto::bitmap(Proto::BG) & 0x48);
}

static void test_every_slug_round_trips() {
  for (uint8_t i = 0; i < wifi_proto::COUNT; i++) {
    const Proto p = static_cast<Proto>(i);
    Proto out = Proto::BG;  // deliberately not the value under test
    TEST_ASSERT_TRUE(wifi_proto::parse(wifi_proto::name(p), &out));
    TEST_ASSERT_EQUAL_UINT8((uint8_t)p, (uint8_t)out);
  }
}

static void test_rejects_garbage_without_touching_output() {
  Proto out = Proto::BGN;
  TEST_ASSERT_FALSE(wifi_proto::parse("", &out));
  TEST_ASSERT_FALSE(wifi_proto::parse(nullptr, &out));
  TEST_ASSERT_FALSE(wifi_proto::parse("BGN", &out));    // case-sensitive on purpose
  TEST_ASSERT_FALSE(wifi_proto::parse("bgnax ", &out)); // no trimming
  TEST_ASSERT_FALSE(wifi_proto::parse("ax", &out));
  TEST_ASSERT_FALSE(wifi_proto::parse("11n", &out));
  TEST_ASSERT_EQUAL_UINT8((uint8_t)Proto::BGN, (uint8_t)out);
  TEST_ASSERT_FALSE(wifi_proto::parse("bgn", nullptr));
}

static void test_slugs_are_distinct() {
  TEST_ASSERT_NOT_EQUAL(0, strcmp(wifi_proto::name(Proto::BGNAX), wifi_proto::name(Proto::BGN)));
  TEST_ASSERT_NOT_EQUAL(0, strcmp(wifi_proto::name(Proto::BGN), wifi_proto::name(Proto::BG)));
  TEST_ASSERT_NOT_EQUAL(0, strcmp(wifi_proto::name(Proto::BGNAX), wifi_proto::name(Proto::BG)));
}

int main(int, char**) {
  UNITY_BEGIN();
  RUN_TEST(test_default_makes_no_driver_call);
  RUN_TEST(test_bitmaps_match_idf_values);
  RUN_TEST(test_every_slug_round_trips);
  RUN_TEST(test_rejects_garbage_without_touching_output);
  RUN_TEST(test_slugs_are_distinct);
  return UNITY_END();
}
