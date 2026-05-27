"""Constants for the Actron SHQ integration."""

DOMAIN = "actron_shq"

BASE_POLL_SECONDS = 30
BURST_POLL_SECONDS = 1
BURST_DURATION_SECONDS = 15
RATE_LIMIT_COOLDOWN_SECONDS = 60

# Zone on/off commands: the cloud is slow and sometimes silently accepts then
# drops these, so re-issue the command on an interval until the controller
# confirms the change (rather than firing once and only polling).
ZONE_RESEND_INTERVAL_SECONDS = 5
ZONE_CONFIRM_TIMEOUT_SECONDS = 90
ZONE_CONFIRM_POLL_SECONDS = 1

# Turning off the last active zone shuts the whole system down and latches it
# off until a zone is re-enabled. When a turn-off would leave no other zone
# active on the controller, hold it and monitor for another zone activating for
# up to this long before refusing the turn-off.
LAST_ZONE_OFF_HOLD_SECONDS = 60
