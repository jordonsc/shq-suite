use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

/// WebSocket server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebSocketConfig {
    /// Host address to bind to (e.g., "0.0.0.0" for all interfaces)
    pub host: String,
    /// Port to listen on
    pub port: u16,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".to_string(),
            port: 8766,
        }
    }
}

/// CNC connection type
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum CncConnection {
    Tcp { host: String, port: u16 },
    Serial { port: String, baud_rate: u32 },
}

impl Default for CncConnection {
    fn default() -> Self {
        Self::Tcp {
            host: "192.168.1.100".to_string(),
            port: 23,
        }
    }
}

/// Door configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DoorConfig {
    /// Distance to open the door in millimeters
    pub open_distance: f64,

    /// Speed when opening the door in mm/min
    pub open_speed: f64,

    /// Speed when closing the door in mm/min
    pub close_speed: f64,

    /// CNC axis for door movement (X, Y, Z, A, B, or C)
    pub cnc_axis: String,

    /// Direction to move when opening: "left" or "right"
    /// - "right": Move in positive direction (e.g., 0 -> +1000)
    /// - "left": Move in negative direction (e.g., 0 -> -1000)
    pub open_direction: String,

    /// CNC controller connection
    pub cnc_connection: CncConnection,
}

impl Default for DoorConfig {
    fn default() -> Self {
        Self {
            open_distance: 1000.0,
            open_speed: 6000.0,
            close_speed: 4000.0,
            cnc_axis: "X".to_string(),
            open_direction: "right".to_string(),
            cnc_connection: CncConnection::default(),
        }
    }
}

/// Application configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub door: DoorConfig,
    pub websocket: WebSocketConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            door: DoorConfig::default(),
            websocket: WebSocketConfig::default(),
        }
    }
}

/// Configuration manager for persistent storage
pub struct ConfigManager {
    config_path: PathBuf,
    config: Config,
}

impl ConfigManager {
    /// Create a new configuration manager and load config from disk
    pub async fn new() -> Result<Self> {
        let config_path = Self::get_config_path()?;

        // Ensure config directory exists
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)
                .await
                .context("Failed to create config directory")?;
        }

        // Load or create default config
        let config = Self::load_config(&config_path).await?;

        Ok(Self {
            config_path,
            config,
        })
    }

    /// Get the XDG-compliant config path: ~/.config/dosa/config.yaml
    fn get_config_path() -> Result<PathBuf> {
        let proj_dirs = ProjectDirs::from("", "", "dosa")
            .context("Failed to determine config directory")?;

        Ok(proj_dirs.config_dir().join("config.yaml"))
    }

    /// Load config from disk, or create default if it doesn't exist
    async fn load_config(path: &PathBuf) -> Result<Config> {
        if path.exists() {
            let contents = fs::read_to_string(path)
                .await
                .context("Failed to read config file")?;

            let config: Config = serde_yaml::from_str(&contents)
                .context("Failed to parse config file")?;

            tracing::info!("Loaded configuration from {:?}", path);
            Ok(config)
        } else {
            tracing::info!("Config file not found, creating default at {:?}", path);
            let config = Config::default();

            // Save default config
            let yaml = serde_yaml::to_string(&config)
                .context("Failed to serialize default config")?;
            fs::write(path, yaml)
                .await
                .context("Failed to write default config")?;

            Ok(config)
        }
    }

    /// Save config to disk
    async fn save(&self) -> Result<()> {
        let yaml = serde_yaml::to_string(&self.config)
            .context("Failed to serialize config")?;

        fs::write(&self.config_path, yaml)
            .await
            .context("Failed to write config file")?;

        tracing::debug!("Saved configuration to {:?}", self.config_path);
        Ok(())
    }

    /// Get the current door configuration
    pub fn get_door_config(&self) -> DoorConfig {
        self.config.door.clone()
    }

    /// Set and persist door configuration
    pub async fn set_door_config(&mut self, config: DoorConfig) -> Result<()> {
        self.config.door = config;
        self.save().await?;
        Ok(())
    }

    /// Get the WebSocket configuration
    pub fn get_websocket_config(&self) -> WebSocketConfig {
        self.config.websocket.clone()
    }
}
