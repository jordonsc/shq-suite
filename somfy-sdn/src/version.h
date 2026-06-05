#pragma once

// Firmware semantic version. **Bump this on every change that gets flashed/OTA'd** (see
// CLAUDE.md "Versioning"). Reported in the `/stats` `fw=` field and the mDNS `fw` TXT record;
// the build date/time is appended at the report sites for OTA-flash verification.
//
//   MAJOR — breaking WS/HTTP API or protocol change
//   MINOR — new feature / capability (back-compatible)
//   PATCH — bug fix / internal change
#define SOMFY_FW_VERSION "1.1.0"
