use serde::{Deserialize, Serialize, Serializer};

use crate::config::DoorConfig;

/// Serialize f64 with 3 decimal places to avoid floating point rounding issues
fn round_to_3dp<S>(value: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_f64((*value * 1000.0).round() / 1000.0)
}

/// Client-to-server command messages
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Open the door
    Open,
    /// Close the door
    Close,
    /// Home the door (move to limit switch and set as closed position)
    Home,
    /// Zero the door (set current position as home without homing sequence)
    Zero,
    /// Clear CNC alarm state
    ClearAlarm,
    /// Get current door position and state
    Status,
    /// Set door configuration
    SetConfig {
        open_distance: Option<f64>,
        open_speed: Option<f64>,
        close_speed: Option<f64>,
        cnc_axis: Option<String>,
        limit_offset: Option<f64>,
        open_direction: Option<String>,
    },
    /// Get door configuration
    GetConfig,
    /// Emergency stop
    Stop,
    /// Query all CNC settings
    GetCncSettings,
    /// Get a specific CNC setting
    GetCncSetting {
        setting: String,
    },
    /// Set a specific CNC setting
    SetCncSetting {
        setting: String,
        value: String,
    },
    /// No operation (keep-alive)
    Noop,
}

/// Server-to-client response messages
#[derive(Debug, Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Door status update
    Status {
        version: String,
        door: DoorStatus,
    },
    /// Command response
    Response {
        success: bool,
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        config: Option<DoorConfig>,
    },
    /// CNC settings response (sorted numerically by setting number)
    CncSettings {
        settings: indexmap::IndexMap<String, String>,
    },
    /// CNC setting response
    CncSetting {
        setting: String,
        value: String,
    },
    /// Error message
    Error {
        message: String,
    },
}

/// Door state
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DoorState {
    /// Door is not yet homed (pending initialization)
    Pending,
    /// Door is closed (at home position)
    Closed,
    /// Door is open (at open position)
    Open,
    /// Door is at an intermediate position (neither fully open nor closed)
    Intermediate,
    /// Door is halting (stopping movement)
    Halting,
    /// Door is opening
    Opening,
    /// Door is closing
    Closing,
    /// Door is homing
    Homing,
    /// CNC controller is in alarm state
    Alarm,
    /// System is in fault state (connection error)
    Fault,
}

/// Door position information
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DoorStatus {
    /// Current door state
    pub state: DoorState,
    /// Current position in millimeters relative to home (0 = closed/home position)
    /// Returns 0 if not yet homed
    #[serde(serialize_with = "round_to_3dp")]
    pub position_mm: f64,
    /// Error message if in fault state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault_message: Option<String>,
    /// Alarm code if in alarm state
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm_code: Option<String>,
}
