"""Constants for the UniFi Protect websocket watchdog."""

DOMAIN = "protect_watchdog"

CONF_NVR_HOST = "nvr_host"
CONF_NVR_PORT = "nvr_port"
CONF_STALE_AFTER = "stale_after"

DEFAULT_NVR_PORT = 443
DEFAULT_SCAN_INTERVAL = 60
# Healthy is sub-5s: the event stream carries ~2 kB/s of camera telemetry even
# with nothing moving in the house. 300s is a 60x margin over the worst healthy
# sample measured, so this cannot fire on a quiet estate.
DEFAULT_STALE_AFTER = 300
