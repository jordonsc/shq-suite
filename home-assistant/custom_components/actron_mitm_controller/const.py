"""Constants for the Actron MITM Controller integration."""

DOMAIN = "actron_mitm_controller"

CONF_HOST = "host"
CONF_PORT = "port"

DEFAULT_PORT = 8767

# Grace period (s) for the ESP32 server to publish a transition; we match server-side
# AVAILABILITY_TIMEOUT to the heartbeat cadence the firmware uses (10s).
AVAILABILITY_TIMEOUT_S = 30
# Reconnect backoff (ledger shq-suite-0046). The old flat 30 s sat ABOVE the 30 s availability
# timeout, so every session death — even one a fresh socket would have healed in 2 s — was a
# guaranteed 16 s+ entity outage (754 s/day on the worst somfy controller). Now 2 s after any
# session that delivered at least one frame, doubling towards 30 s only while attempts keep
# failing (a refused connect, or a socket that died before its first frame) — the one case where
# hammering a device would only fill its five WS slots. The availability timeout is NOT masked.
RECONNECT_DELAY_MIN_S = 2
RECONNECT_DELAY_MAX_S = 30
CONNECT_TIMEOUT_S = 5       # how long a single TCP/WS connect attempt may take
COMMAND_RESPONSE_TIMEOUT_S = 10

# ---- diagnostics (firmware diag.{h,cpp}, ledger shq-suite-0038) ----------------------------
#
# The firmware classifies every WS disconnect and stamps it with the machine's condition at that
# instant. Those records arrive here as `diag` messages (live) and `diag_backlog` (replayed on
# connect — a socket cannot be told about its own death, so the backlog is the ONLY way we learn
# why the previous session ended). fw >= 1.14.0 sends the backlog a record per `diag` frame
# instead and splits the vitals into `health` / `health_ws` / `health_net` (each under the
# firmware's 600 B frame budget); the coordinator merges them into one dict.

# Bus event fired for each firmware diagnostic record, and for our own end of the connection.
# Fires on HA's event bus, so it lands in the logbook and can drive automations/alerts.
EVENT_DIAG = "actron_mitm_diag"

# Firmware `health` push cadence — used only to decide when a health payload is considered stale.
HEALTH_INTERVAL_S = 30

# Records already seen, kept so a backlog replay doesn't double-fire events. The firmware ring
# holds 48; tracking a little more than that covers a reconnect burst without unbounded growth.
DIAG_SEEN_MAX = 128

# `source` field on EVENT_DIAG, distinguishing what the firmware observed from what we observed.
SOURCE_DEVICE = "device"
SOURCE_HA = "ha"

# Map firmware mode strings to HA HVACMode values. Done as a dict to avoid HA imports here.
MODES_FW_TO_HA = {
    "off": "off",
    "cool": "cool",
    "heat": "heat",
    "auto": "heat_cool",
    "fan": "fan_only",
}
MODES_HA_TO_FW = {v: k for k, v in MODES_FW_TO_HA.items()}

FAN_FW_TO_HA = {
    "low": "low",
    "med": "medium",
    "high": "high",
    "auto": "auto",
}
FAN_HA_TO_FW = {v: k for k, v in FAN_FW_TO_HA.items()}
