"""Constants for the Somfy SDN integration."""

DOMAIN = "somfy_sdn"

CONF_HOST = "host"
CONF_PORT = "port"

DEFAULT_PORT = 8767

# Availability follows the firmware's ~10 s state-push heartbeat; flip unavailable after 30 s
# of silence (coordinator pattern, mirrors the Actron MITM controller).
AVAILABILITY_TIMEOUT_S = 30
RECONNECT_DELAY_S = 30
CONNECT_TIMEOUT_S = 5
COMMAND_RESPONSE_TIMEOUT_S = 10

# ---- diagnostics (firmware diag.{h,cpp}, ledger shq-suite-0038) ----------------------------
#
# The firmware classifies every WS disconnect and stamps it with the machine's condition at that
# instant. Those records arrive as `diag` messages (live) and `diag_backlog` (replayed on connect —
# a socket cannot be told about its own death, so the backlog is the ONLY way we learn why the
# previous session ended). `health` carries the periodic vitals.
#
# Ported from actron_mitm_controller 1.2.0. This fleet is where it matters most: Bed 4 was caught
# in a locked 40 s unavailable flap with the heartbeat firing normally, and nothing on the device
# could say why. NOTE that 40 s is OUR arithmetic (AVAILABILITY_TIMEOUT_S + the 10 s monitor tick),
# not a device signature.

EVENT_DIAG = "somfy_sdn_diag"

# Records already turned into bus events. The firmware ring holds 48 and replays a backlog on every
# connect; tracking more than that covers a reconnect burst without unbounded growth.
DIAG_SEEN_MAX = 128

# `source` on EVENT_DIAG — what the firmware observed vs what we observed.
SOURCE_DEVICE = "device"
SOURCE_HA = "ha"
