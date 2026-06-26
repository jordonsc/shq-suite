// Native ESP-IDF app-descriptor override (Option A) — twin of somfy-sdn/src/app_desc.cpp.
//
// Sets the image's native esp_app_desc project_name to "actron-mitm" so it can be positively
// distinguished from the somfy-sdn firmware (same TinyC6 board, same /update OTA endpoint, same
// MAC OUI). Read by esp_ota_get_partition_description() in the OTA guard (handleUpdate) and by
// external tooling. Without this both images report project_name="arduino-lib-builder".
//
// Strong `extern "C"` symbol in section .rodata_desc → the linker resolves esp_app_desc from
// here and never pulls the prebuilt arduino-lib-builder copy from libesp_app_format.a. This
// firmware has no semver macro, so version carries the build date.

#include <esp_app_desc.h>

extern "C" const __attribute__((section(".rodata_desc"))) __attribute__((used))
esp_app_desc_t esp_app_desc = {
    .magic_word = ESP_APP_DESC_MAGIC_WORD,
    .version = __DATE__,
    .project_name = "actron-mitm",
    .time = __TIME__,
    .date = __DATE__,
};
