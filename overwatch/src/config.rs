use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub alarms: HashMap<String, PathBuf>,
    pub notification_tones: HashMap<String, PathBuf>,
    #[serde(default = "default_server_address")]
    pub server_address: String,
    #[serde(default = "default_voice")]
    pub default_voice: String,
    #[serde(default = "default_engine")]
    pub default_engine: String,
    #[serde(default = "default_volume")]
    pub default_volume: f32,
    pub aws: Option<AwsConfig>,
}

fn default_voice() -> String {
    "Amy".to_string()
}

fn default_engine() -> String {
    "generative".to_string()
}

fn default_volume() -> f32 {
    1.0
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AwsConfig {
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

fn default_server_address() -> String {
    "0.0.0.0:50051".to_string()
}

impl Config {
    pub fn from_file(path: &str) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    pub fn get_alarm(&self, alarm_id: &str) -> Option<&PathBuf> {
        self.alarms.get(alarm_id)
    }

    pub fn get_notification_tone(&self, tone_id: &str) -> Option<&PathBuf> {
        self.notification_tones.get(tone_id)
    }
}
