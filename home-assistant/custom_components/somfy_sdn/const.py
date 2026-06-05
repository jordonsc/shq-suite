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
