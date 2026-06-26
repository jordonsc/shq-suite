// Native ESP-IDF app-descriptor override (Option A — see somfy-sdn/CLAUDE.md "Device identity").
//
// The prebuilt Arduino core ships an `esp_app_desc` in libesp_app_format.a with
// project_name="arduino-lib-builder" and version=<arduino git hash> — useless for telling our
// firmwares apart. The linker only pulls that archive member to resolve the `esp_app_desc`
// symbol; by defining our OWN strong `esp_app_desc` here we satisfy the symbol first, so the
// prebuilt one is never linked and the image's NATIVE descriptor reports our real app id +
// version. That descriptor is what `esp_ota_get_partition_description()` reads in the OTA guard
// (http_api.cpp handleUpdate), and what `esptool image_info` / any external tool sees.
//
// Must be `extern "C"` (unmangled symbol) and live in section `.rodata_desc` so the linker
// script (ld/sections.ld `.flash.appdesc`) places it at the fixed offset after the image header.
// Designated initialisers are in declaration order; omitted fields (secure_version, reserv1,
// idf_ver, app_elf_sha256, reserved) zero-initialise — fine, none are load-bearing for us.

#include <esp_app_desc.h>

#include "version.h"

extern "C" const __attribute__((section(".rodata_desc"))) __attribute__((used))
esp_app_desc_t esp_app_desc = {
    .magic_word = ESP_APP_DESC_MAGIC_WORD,
    .version = SOMFY_FW_VERSION,
    .project_name = "somfy-sdn",
    .time = __TIME__,
    .date = __DATE__,
};
