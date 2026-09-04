// Native ESP-IDF app-descriptor override (Option A) — twin of somfy-sdn/src/app_desc.cpp.
//
// Sets the image's native esp_app_desc project_name to "actron-mitm" so it can be positively
// distinguished from the somfy-sdn firmware (same TinyC6 board, same /update OTA endpoint, same
// MAC OUI). Read by esp_ota_get_partition_description() in the OTA guard (handleUpdate) and by
// external tooling. Without this both images report project_name="arduino-lib-builder".
//
// Strong `extern "C"` symbol in section .rodata_desc → the linker resolves esp_app_desc from
// here and never pulls the prebuilt arduino-lib-builder copy from libesp_app_format.a. This
// The descriptor's `version` now carries ACTRON_FW_VERSION (fw 1.10.0), matching the somfy twin.
// This also closes a caching trap that used to need a manual workaround: PlatformIO caches object
// files by content hash, so while `version` was `__DATE__` an unchanged app_desc.cpp kept its OLD
// build date even when the rest of the image rebuilt, leaving the descriptor disagreeing with the
// `fw=` string in /stats. Including version.h means a semver bump now recompiles this file, which
// refreshes the date as a side effect — bump the version and the descriptor follows.

#include <esp_app_desc.h>

#include "version.h"

extern "C" const __attribute__((section(".rodata_desc"))) __attribute__((used))
esp_app_desc_t esp_app_desc = {
    .magic_word = ESP_APP_DESC_MAGIC_WORD,
    .version = ACTRON_FW_VERSION,
    .project_name = "actron-mitm",
    .time = __TIME__,
    .date = __DATE__,
};
