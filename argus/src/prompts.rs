//! LLM prompt bundle — loaded from `prompts.yaml` (the single source of truth for
//! everything the models read).
//!
//! The repo's `prompts.yaml` is **baked into the binary** at build time
//! (`include_str!`) as the default, so a deployed binary always has working
//! prompts. At startup, if `~/.config/argus/prompts.yaml` exists (the host config
//! dir, bind-mounted into the container), it is loaded INSTEAD — so prompt wording
//! can be tuned by editing that file + restarting the service, with no rebuild.
//!
//! Only the static `system`/`alarm`/`investigate_*`/`identify` text and the
//! `context`/`live_message` TEMPLATES live here; the engine substitutes the
//! `{...}` placeholders at runtime (see `engine::live_instruction_camera`).

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::Deserialize;
use tracing::info;

/// The repo `prompts.yaml`, baked in as the default.
const DEFAULT_PROMPTS_YAML: &str = include_str!("../prompts.yaml");

/// Every prompt Argus sends to the models, deserialised from `prompts.yaml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Prompts {
    /// Universal system block (prepended to the seed for the live Sonnet calls).
    pub system: String,
    /// Live-message opener for an `Alarm` case.
    pub alarm: String,
    /// Live-message opener for an `Investigate` case, trigger camera.
    pub investigate_trigger: String,
    /// Live-message opener for an `Investigate` case, ancillary cameras.
    pub investigate_ancillary: String,
    /// Priors block; `{local_time}` / `{arm_state}` filled at runtime.
    pub context: String,
    /// Per-camera live-message wrapper; `{opener}` / `{label}` / `{context}` /
    /// `{telemetry}` / `{roster}` filled at runtime.
    pub live_message: String,
    /// Opus forensic identification instruction (its system block is the seed alone).
    pub identify: String,
}

impl Prompts {
    /// Load the prompt bundle: a runtime override at `~/.config/argus/prompts.yaml`
    /// if present, else the baked-in default. Parse errors are fatal (a malformed
    /// prompt file must not silently fall back to stale behaviour).
    pub fn load() -> Result<Self> {
        if let Some(dirs) = ProjectDirs::from("", "", "argus") {
            let path = dirs.config_dir().join("prompts.yaml");
            if path.exists() {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading prompt overrides {}", path.display()))?;
                info!("Loaded prompt overrides from {}", path.display());
                return serde_yaml::from_str(&text)
                    .with_context(|| format!("parsing prompt overrides {}", path.display()));
            }
        }
        serde_yaml::from_str(DEFAULT_PROMPTS_YAML).context("parsing baked-in prompts.yaml")
    }

    /// Build the live (Sonnet) `system` block: the universal system prompt followed
    /// by the private premises seed (which supplies the residence/residents summaries
    /// the prompt refers to).
    pub fn system_block(&self, seed: &str) -> String {
        format!("{}\n\n{}", self.system.trim_end(), seed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The baked-in prompts.yaml must always parse and carry every field.
    #[test]
    fn baked_prompts_parse() {
        let p: Prompts = serde_yaml::from_str(DEFAULT_PROMPTS_YAML).expect("baked prompts parse");
        assert!(p.system.contains("smart-security AI"));
        assert!(p.alarm.contains("intrusion is assumed"));
        assert!(p.investigate_trigger.contains("benign"));
        assert!(p.identify.contains("weapon_confidence"));
        // The templates must still carry their placeholders for the engine to fill.
        assert!(p.context.contains("{local_time}") && p.context.contains("{arm_state}"));
        for ph in ["{opener}", "{label}", "{context}", "{telemetry}", "{roster}"] {
            assert!(p.live_message.contains(ph), "live_message missing {ph}");
        }
    }
}
