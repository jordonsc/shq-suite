#pragma once

// Firmware semantic version. **Bump this on every change that gets flashed/OTA'd** (see
// CLAUDE.md "Versioning"). Reported in the `/stats` `fw=` field; the build date/time is
// appended at the report site for OTA-flash verification.
//
//   MAJOR — breaking WS/HTTP API or protocol change
//   MINOR — new feature / capability (back-compatible)
//   PATCH — bug fix / internal change
//
// Introduced at 1.10.0 rather than 1.0.0 (fw 1.10.0, ledger shq-suite-0041). This firmware
// previously reported only its build date, which made "did the OTA take?" a question about
// timestamps. The number is deliberately kept in step with somfy-sdn: the two are design twins
// sharing mono/fault/diag/ws_guard verbatim, and a shared generation number is what makes
// "are the twins in step?" answerable at a glance.
#define ACTRON_FW_VERSION "1.14.2"
