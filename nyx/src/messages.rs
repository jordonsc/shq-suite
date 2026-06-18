use serde::{Deserialize, Serialize};

/// What the display does once `auto_off_time` idle elapses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum IdleMode {
    /// Turn the backlight off (the default; behaviour all kiosks have today).
    #[default]
    Off,
    /// Keep the backlight at `dim_level` and show the Chronos clock overlay.
    Clock,
}

/// Client-to-server command messages
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    SetDisplay { state: bool },
    SetBrightness { brightness: u8 },
    GetMetrics,
    SetAutoDimConfig {
        dim_level: u8,
        bright_level: u8,
        auto_dim_time: u32,
        auto_off_time: u32,
        // Optional so existing clients (which never send it) keep working.
        #[serde(default)]
        idle_mode: IdleMode,
    },
    GetAutoDimConfig,
    Wake,
    Sleep,
    Navigate { url: String },
    GetUrl,
    Noop,
}

/// Server-to-client response messages
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    Metrics {
        version: String,
        display: DisplayMetrics,
        auto_dim: AutoDimStatus,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Response {
        success: bool,
        command: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        config: Option<AutoDimConfig>,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Error {
        message: String,
    },
}

/// Display state and brightness
#[derive(Debug, Clone, Serialize)]
pub struct DisplayMetrics {
    pub display_on: bool,
    pub brightness: u8,
}

/// Auto-dim configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoDimConfig {
    pub dim_level: u8,
    pub bright_level: u8,
    pub auto_dim_time: u32,
    pub auto_off_time: u32,
    /// What to do at the auto-off point. Defaults to `Off` so existing config
    /// files and other kiosks are unchanged.
    #[serde(default)]
    pub idle_mode: IdleMode,
}

impl Default for AutoDimConfig {
    fn default() -> Self {
        Self {
            dim_level: 25,      // ~10% brightness
            bright_level: 178,  // ~70% brightness
            auto_dim_time: 0,   // 0 = disabled
            auto_off_time: 0,   // 0 = disabled
            idle_mode: IdleMode::Off,
        }
    }
}

/// Auto-dim runtime status
#[derive(Debug, Clone, Serialize)]
pub struct AutoDimStatus {
    pub dim_level: u8,
    pub bright_level: u8,
    pub auto_dim_time: u32,
    pub auto_off_time: u32,
    pub is_dimmed: bool,
    pub last_touch_time: f64,
    pub idle_mode: IdleMode,
}
