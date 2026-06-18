//! Argus — AI-powered alarm assessment daemon.
//!
//! When `alarm_control_panel.shq_alarm` transitions to `triggered`, Argus opens
//! a case and runs a multi-camera + telemetry assessment loop: the **Sonnet**
//! live what/where loop builds an evolving structured [`CaseState`], and the
//! **Opus** forensic pass firms up intruder identifications on the best stills.
//! The case is journalled to disk and broadcast for downstream consumers
//! (Phase 2a offsite replication, Phase 3 voice/PagerDuty, Phase 4 kiosk HUD).
//!
//! `--once` runs a single assessment cycle without the alarm and prints the
//! resulting `CaseState` (tests the HA + tiered-Anthropic path without arming).

mod case;
mod config;
mod engine;
mod ha;
mod llm;
mod state;
mod version;

use anyhow::{Context, Result};
use clap::Parser;
use std::path::PathBuf;
use tokio::sync::{mpsc, watch};
use tracing::info;
use tracing_subscriber::EnvFilter;

use config::Config;
use engine::Engine;
use ha::{HaEvent, RestClient};
use llm::AnthropicClient;

#[derive(Parser, Debug)]
#[command(name = "argus", version, about = "Argus — AI-powered alarm assessment daemon")]
struct Cli {
    /// Path to config.yaml (default: ~/.config/argus/config.yaml).
    #[arg(long)]
    config: Option<PathBuf>,

    /// Run a single assessment cycle now (no alarm) and print the CaseState.
    #[arg(long)]
    once: bool,
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
    let sonnet = AnthropicClient::new(
        cfg.anthropic.api_key.clone(),
        cfg.anthropic.live_model.clone(),
    )?;
    let opus = AnthropicClient::new(
        cfg.anthropic.api_key.clone(),
        cfg.anthropic.id_model.clone(),
    )?;

    // Broadcast latest CaseState (Phases 3/4 subscribe in-process).
    let (state_tx, _state_rx) = watch::channel::<Option<case::CaseState>>(None);

    let mut eng = Engine::new(cfg.clone(), rest, sonnet, opus, seed, state_tx);

    if cli.once {
        eng.run_once().await?;
        return Ok(());
    }

    run_daemon(cfg, eng).await
}

/// Subscribe to the HA alarm and drive the engine until Ctrl-C.
async fn run_daemon(cfg: Config, eng: Engine) -> Result<()> {
    let (tx, rx) = mpsc::channel::<HaEvent>(16);

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

    // Run the engine; cancel on Ctrl-C.
    let engine_task = tokio::spawn(eng.run(rx));
    tokio::signal::ctrl_c().await.ok();
    info!("Shutdown requested");
    engine_task.abort();
    Ok(())
}
