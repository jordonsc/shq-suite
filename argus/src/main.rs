//! Argus — AI-powered alarm assessment daemon (Phase 1).
//!
//! When `alarm_control_panel.shq_alarm` transitions to `triggered`, Argus
//! captures a camera still, sends it to Claude with the premises seed, and logs
//! a natural-language assessment. `--once` skips the alarm wait and assesses a
//! camera immediately (tests the HA-REST + Anthropic path without arming).

mod config;
mod ha;
mod llm;
mod state;
mod version;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use config::Config;
use ha::{HaEvent, RestClient};
use llm::AnthropicClient;

#[derive(Parser, Debug)]
#[command(name = "argus", version, about = "Argus — AI-powered alarm assessment daemon")]
struct Cli {
    /// Path to config.yaml (default: ~/.config/argus/config.yaml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Assess a camera once and exit, without waiting for the alarm.
    #[arg(long)]
    once: bool,

    /// Camera entity to use with --once (default: the first configured camera).
    #[arg(long)]
    camera: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| "argus=info".into()),
        )
        .init();

    info!("Starting Argus v{}", version::ARGUS_VERSION);

    let cfg = Config::load(cli.config.clone())?;
    let seed = cfg.load_seed().context("failed to load premises seed")?;
    info!("Loaded premises seed ({} bytes) from {}", seed.len(), cfg.seed_path);

    let rest = RestClient::new(&cfg.ha.url, &cfg.ha.token)?;
    let llm = AnthropicClient::new(cfg.anthropic.api_key.clone(), cfg.anthropic.live_model.clone())?;

    if cli.once {
        let (entity, label) = resolve_camera(&cfg, cli.camera.as_deref())?;
        assess_camera(&rest, &llm, &seed, &entity, &label).await?;
        return Ok(());
    }

    run_daemon(cfg, rest, llm, seed).await
}

/// Subscribe to the alarm and assess on every trigger transition until Ctrl-C.
async fn run_daemon(
    cfg: Config,
    rest: RestClient,
    llm: AnthropicClient,
    seed: String,
) -> Result<()> {
    let (tx, mut rx) = mpsc::channel::<HaEvent>(16);

    let ws_url = ha::websocket_url(&cfg.ha.url);
    let token = cfg.ha.token.clone();
    let alarm_entity = cfg.alarm_entity.clone();
    tokio::spawn(async move {
        ha::ws::run(ws_url, token, alarm_entity, tx).await;
    });

    info!(
        "Watching {} for trigger transitions; press Ctrl-C to stop",
        cfg.alarm_entity
    );

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown requested");
                break;
            }
            maybe_event = rx.recv() => {
                let Some(event) = maybe_event else {
                    error!("event channel closed unexpectedly; exiting");
                    break;
                };
                match event {
                    HaEvent::AlarmTriggered { entity_id } => {
                        info!("Alarm triggered ({entity_id}); running assessment");
                        for cam in &cfg.cameras {
                            if let Err(e) =
                                assess_camera(&rest, &llm, &seed, &cam.entity, &cam.label).await
                            {
                                error!("assessment failed for {}: {e:#}", cam.label);
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Capture one still and log the resulting assessment + token usage.
async fn assess_camera(
    rest: &RestClient,
    llm: &AnthropicClient,
    seed: &str,
    entity: &str,
    label: &str,
) -> Result<()> {
    info!("Capturing still from {label} ({entity})");
    let jpeg = rest.snapshot(entity).await?;
    info!(
        "Captured {} bytes; assessing with {}",
        jpeg.len(),
        llm.model()
    );

    let assessment = llm.assess(seed, label, &jpeg).await?;

    info!(camera = %label, "ASSESSMENT: {}", assessment.text);
    info!(
        input = assessment.usage.input_tokens,
        output = assessment.usage.output_tokens,
        cache_write = assessment.usage.cache_creation_input_tokens,
        cache_read = assessment.usage.cache_read_input_tokens,
        "token usage"
    );
    Ok(())
}

/// Resolve the (entity, label) to assess for `--once`: an explicit `--camera`
/// (label looked up from config, falling back to the entity id) or the first
/// configured camera.
fn resolve_camera(cfg: &Config, camera_override: Option<&str>) -> Result<(String, String)> {
    match camera_override {
        Some(entity) => {
            let label = cfg
                .cameras
                .iter()
                .find(|c| c.entity == entity)
                .map(|c| c.label.clone())
                .unwrap_or_else(|| entity.to_string());
            Ok((entity.to_string(), label))
        }
        None => {
            let cam = cfg
                .cameras
                .first()
                .context("no cameras configured; pass --camera or add one to config.yaml")?;
            Ok((cam.entity.clone(), cam.label.clone()))
        }
    }
}
