//! Configuration loading for Argus.
//!
//! Config lives at `~/.config/argus/config.yaml` (override with `--config`).
//! Secrets are kept out of the file: any `${VAR}` in the YAML is expanded from
//! the environment at load time (systemd `EnvironmentFile`, or the shell).

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use directories::{BaseDirs, ProjectDirs};
use serde::Deserialize;

/// Top-level Argus configuration (`config.yaml`).
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub ha: HaConfig,
    pub anthropic: AnthropicConfig,
    /// Path to the private premises seed (env/`~` expanded).
    pub seed_path: String,
    /// Cameras Argus captures each assessment tick.
    pub cameras: Vec<CameraConfig>,
    /// The HA alarm entity Argus subscribes to.
    pub alarm_entity: String,
    /// The assessment-loop cadence + guardrails (Phase 2).
    #[serde(default)]
    pub loop_config: LoopConfig,
    /// Security-relevant HA entities pulled as per-tick telemetry (door/window/
    /// motion/reed/DOSA). Rendered as text after the cached seed.
    #[serde(default)]
    pub telemetry_entities: Vec<String>,
}

/// Assessment-loop cadence and cost guardrails (Phase 2).
#[derive(Debug, Clone, Deserialize)]
pub struct LoopConfig {
    /// Seconds between assessment ticks while the alarm is triggered.
    #[serde(default = "default_cadence")]
    pub cadence_secs: u64,
    /// Optional daily billable-input-token cap. When exceeded, the loop slows to
    /// `slow_cadence_secs` until UTC midnight. `null`/absent = no cap.
    #[serde(default)]
    pub daily_token_cap: Option<u64>,
    /// Cadence used once the daily cap is hit.
    #[serde(default = "default_slow_cadence")]
    pub slow_cadence_secs: u64,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            cadence_secs: default_cadence(),
            daily_token_cap: None,
            slow_cadence_secs: default_slow_cadence(),
        }
    }
}

fn default_cadence() -> u64 {
    6
}

fn default_slow_cadence() -> u64 {
    30
}

/// Home Assistant connection (localhost on atlas).
#[derive(Debug, Clone, Deserialize)]
pub struct HaConfig {
    /// Base URL, e.g. `http://localhost:8123`.
    pub url: String,
    /// Long-lived access token (minted in the HA UI). Keep in the environment.
    pub token: String,
}

/// Anthropic models + key.
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicConfig {
    pub api_key: String,
    /// The live what/where loop model (Phase 1 uses this one).
    pub live_model: String,
    /// Reserved for Phase 2 forensic identification (parsed now so the config
    /// shape is stable; not yet read).
    #[serde(default = "default_id_model")]
    #[allow(dead_code)]
    pub id_model: String,
}

fn default_id_model() -> String {
    "claude-opus-4-8".to_string()
}

/// A camera Argus can capture, with a human label for the prompt.
#[derive(Debug, Clone, Deserialize)]
pub struct CameraConfig {
    /// HA camera entity id, e.g. `camera.garage_camera_high_resolution_channel`.
    pub entity: String,
    /// Human label included in the prompt, e.g. `Garage`.
    pub label: String,
}

impl Config {
    /// Load and parse config from `path`, or the default
    /// `~/.config/argus/config.yaml` if `None`. Expands `${VAR}` from the
    /// environment before parsing.
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = match path {
            Some(p) => p,
            None => default_config_path()?,
        };
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let expanded = expand_env(&raw)?;
        let cfg: Config = serde_yaml::from_str(&expanded)
            .with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }

    /// Read the premises seed referenced by `seed_path` (env/`~` expanded).
    pub fn load_seed(&self) -> Result<String> {
        let path = expand_tilde(&self.seed_path);
        std::fs::read_to_string(&path)
            .with_context(|| format!("reading premises seed {}", path.display()))
    }
}

/// `~/.config/argus/config.yaml` (mirrors nyx's `~/.config/shqd/`).
fn default_config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "argus")
        .context("cannot determine config directory")?;
    Ok(dirs.config_dir().join("config.yaml"))
}

/// Expand a leading `~/` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(base) = BaseDirs::new() {
            return base.home_dir().join(rest);
        }
    }
    PathBuf::from(path)
}

/// Expand `${VAR}` references from the environment. Errors on an unterminated
/// `${` or a referenced variable that is not set, so a missing secret fails
/// loudly at startup rather than sending an empty token.
fn expand_env(input: &str) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut name = String::new();
            let mut closed = false;
            for nc in chars.by_ref() {
                if nc == '}' {
                    closed = true;
                    break;
                }
                name.push(nc);
            }
            if !closed {
                return Err(anyhow!("unterminated ${{...}} in config"));
            }
            let value = std::env::var(&name)
                .map_err(|_| anyhow!("environment variable {name} (referenced in config) is not set"))?;
            out.push_str(&value);
        } else {
            out.push(c);
        }
    }
    Ok(out)
}
