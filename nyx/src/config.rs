use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

use crate::messages::AutoDimConfig;

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
            port: 8765,
        }
    }
}

/// Application configuration stored in ~/.config/shqd/config.json
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub auto_dim: AutoDimConfig,
    pub websocket: WebSocketConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            auto_dim: AutoDimConfig::default(),
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

    /// Get the XDG-compliant config path: ~/.config/shqd/config.json
    fn get_config_path() -> Result<PathBuf> {
        let proj_dirs = ProjectDirs::from("", "", "shqd")
            .context("Failed to determine config directory")?;

        Ok(proj_dirs.config_dir().join("config.json"))
    }

    /// Load config from disk, or create default if it doesn't exist
    async fn load_config(path: &PathBuf) -> Result<Config> {
        if path.exists() {
            let contents = fs::read_to_string(path)
                .await
                .context("Failed to read config file")?;

            let config: Config = serde_json::from_str(&contents)
                .context("Failed to parse config file")?;

            tracing::info!("Loaded configuration from {:?}", path);
            Ok(config)
        } else {
            tracing::info!("Config file not found, creating default at {:?}", path);
            let config = Config::default();

            // Save default config
            let json = serde_json::to_string_pretty(&config)
                .context("Failed to serialize default config")?;
            fs::write(path, json)
                .await
                .context("Failed to write default config")?;

            Ok(config)
        }
    }

    /// Save config to disk
    async fn save(&self) -> Result<()> {
        let json = serde_json::to_string_pretty(&self.config)
            .context("Failed to serialize config")?;

        fs::write(&self.config_path, json)
            .await
            .context("Failed to write config file")?;

        tracing::debug!("Saved configuration to {:?}", self.config_path);
        Ok(())
    }

    /// Get the current auto-dim configuration
    pub fn get_auto_dim_config(&self) -> AutoDimConfig {
        self.config.auto_dim.clone()
    }

    /// Set and persist auto-dim configuration
    pub async fn set_auto_dim_config(&mut self, config: AutoDimConfig) -> Result<()> {
        self.config.auto_dim = config;
        self.save().await?;
        Ok(())
    }

    /// Get the WebSocket configuration
    pub fn get_websocket_config(&self) -> WebSocketConfig {
        self.config.websocket.clone()
    }
}
