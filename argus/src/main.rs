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
mod out;
mod state;
mod version;
mod web;

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

    // Broadcast latest CaseState (Phases 2a/3/4 subscribe in-process).
    let (state_tx, _state_rx) = watch::channel::<Option<case::CaseState>>(None);

    // Subscribe BEFORE moving `state_tx` into the engine: the offsite replicator
    // (Phase 2a) uses this receiver as a wake signal. We spawn it only in daemon
    // mode (a `--once` cycle is ephemeral and exits before an upload would matter).
    let offsite_rx = state_tx.subscribe();
    // Same for the Phase 3 outputs consumer (voice + PagerDuty), taken before the
    // sender moves into the engine.
    let outputs_rx = state_tx.subscribe();
    // Phase 4 (kiosk HUD): the web server's WS push and the kiosk-takeover
    // consumer each take a receiver here, before `state_tx` moves into the engine.
    let web_rx = state_tx.subscribe();
    let kiosks_rx = state_tx.subscribe();

    let mut eng = Engine::new(cfg.clone(), rest, sonnet, opus, seed, state_tx);

    if cli.once {
        eng.run_once().await?;
        return Ok(());
    }

    if cfg.offsite.enabled {
        let offsite_cfg = cfg.offsite.clone();
        let base = case::default_case_base()?;
        info!("Spawning offsite S3 replication (Phase 2a)");
        tokio::spawn(out::offsite::run(offsite_cfg, base, offsite_rx));
    }

    // Phase 3 outputs. Voice spawns iff `overwatch:` is configured; PagerDuty iff
    // a non-empty `routing_key` is present. An absent block / empty key disables
    // that channel — neither is a hard blocker. Both log on failure, never crash.
    let voice = cfg.overwatch.as_ref().map(|o| {
        info!(host = %o.host, port = o.port, "Voice output enabled (Overwatch gRPC)");
        out::voice::VoiceClient::new(&o.host, o.port, o.alarm_id.clone(), o.voice_id.clone(), o.volume)
    });
    let pagerduty = cfg.pagerduty.as_ref().and_then(|p| {
        if p.routing_key.trim().is_empty() {
            info!("PagerDuty configured but routing_key empty; PagerDuty output disabled");
            None
        } else {
            info!(source = %p.source, "PagerDuty output enabled (Events v2)");
            Some(out::pagerduty::PagerDuty::new(p.routing_key.clone(), p.source.clone()))
        }
    });
    if voice.is_some() || pagerduty.is_some() {
        info!("Spawning Phase 3 outputs consumer (voice + PagerDuty)");
        tokio::spawn(out::run(outputs_rx, voice, pagerduty, cfg.offsite.clone()));
    }

    // Phase 4 — kiosk HUD. The HUD HTTP/WS server spawns iff `web:` is configured;
    // the kiosk-takeover consumer spawns iff `web:` is configured AND `kiosks:` is
    // non-empty (the takeover URL is `web.public_base`, so no web = nothing to
    // navigate to). Both are daemon-only and never crash the daemon on failure.
    if let Some(web_cfg) = cfg.web.clone() {
        let case_base = case::default_case_base()?;
        info!(bind = %web_cfg.bind, "Spawning Phase 4 HUD server");
        let public_base = web_cfg.public_base.clone();
        tokio::spawn(web::serve(web_cfg, web_rx, case_base));

        if !cfg.kiosks.is_empty() {
            // The takeover consumer drives HA directly, so it needs its own
            // RestClient (the engine owns the one built above).
            let kiosk_rest = RestClient::new(&cfg.ha.url, &cfg.ha.token)?;
            info!(kiosks = cfg.kiosks.len(), "Spawning Phase 4 kiosk-takeover consumer");
            tokio::spawn(out::kiosks::run(
                kiosks_rx,
                cfg.kiosks.clone(),
                kiosk_rest,
                public_base,
            ));
        }
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
