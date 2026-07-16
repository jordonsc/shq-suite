"""Constants for the UniFi Access DPS workaround integration."""

DOMAIN = "unifi_access_dps"

DEFAULT_INPUT_KEY = "input_d1_dps"
EVENT_DEVICE_UPDATE = "access.data.device.update"
EVENT_LOCATION_UPDATE_LEGACY = "access.data.device.location_update_v2"
EVENT_LOCATION_UPDATE_V2 = "access.data.v2.location.update"
EVENT_V2_DEVICE_UPDATE = "access.data.v2.device.update"
SIGNAL_UPDATE = f"{DOMAIN}_update"

LOCK_POLL_SECONDS = 30

CONF_API_TOKEN = "api_token"
CONF_VERIFY_SSL = "verify_ssl"
CONF_DOORS = "doors"
CONF_DEVICE_ID = "device_id"
CONF_DOOR_ID = "door_id"
CONF_INPUT_KEY = "input_key"

SERVICE_LOCK_NOW = "lock_now"
