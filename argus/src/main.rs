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
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use config::Config;
use engine::{ControlCommand, Engine};
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

    /// Trigger-profile override for `--once` (and the manual case it opens):
    /// `alarm` (default), `investigate`, or `general`. Lets the gated/escalation
    /// paths (Phase 4b) be exercised without wiring real softer-trigger HA
    /// entities — a `general`/`investigate` override opens a GATED case so you can
    /// eyeball the gated `CaseState` (no klaxon/voice/PD, no Opus unless escalated).
    #[arg(long, value_parser = parse_profile)]
    profile: Option<case::TriggerProfile>,

    /// Dry-voice mode: run the full voice gate but LOG every line/klaxon instead
    /// of contacting Overwatch (no real TTS or klaxon). Overrides `overwatch:` and
    /// forces the outputs consumer to run. Use it on a live (scratch-alarm) test to
    /// see exactly what WOULD be verbalised.
    #[arg(long)]
    dry_voice: bool,
}

/// clap value-parser for `--profile` (delegates to `TriggerProfile`'s `FromStr`).
fn parse_profile(s: &str) -> std::result::Result<case::TriggerProfile, String> {
    s.parse()
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
    // The LIVE (Sonnet) `system` block = the universal security-AI system prompt
    // followed by the private premises seed (the seed supplies the residence/
    // residents summaries the prompt refers to). The Opus forensic call gets the
    // SEED alone (see Engine.seed) — the live prompt's task framing derailed it.
    let system = format!("{}\n\n{}", engine::SYSTEM_PROMPT, seed);

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

    // Alarm-mode broadcast: arming/armed drive the HUD standby pane + the kiosk
    // takeover even with no active case. Subscribed before the sender moves into
    // the engine (the HUD WS + the takeover consumer each take a receiver).
    let (mode_tx, _mode_rx) = watch::channel::<state::AlarmMode>(state::AlarmMode::Disarmed);
    let outputs_mode_rx = mode_tx.subscribe();
    let web_mode_rx = mode_tx.subscribe();
    let kiosks_mode_rx = mode_tx.subscribe();

    let mut eng = Engine::new(cfg.clone(), rest, sonnet, opus, system, seed, state_tx);

    if cli.once {
        eng.run_once(cli.profile).await?;
        return Ok(());
    }
    eng.set_mode_tx(mode_tx);

    if cfg.offsite.enabled {
        let offsite_cfg = cfg.offsite.clone();
        let base = case::default_case_base()?;
        info!("Spawning offsite S3 replication (Phase 2a)");
        tokio::spawn(out::offsite::run(offsite_cfg, base, offsite_rx));
    }

    // Phase 3 outputs. Voice spawns iff `overwatch:` is configured; PagerDuty iff
    // a non-empty `routing_key` is present. An absent block / empty key disables
    // that channel — neither is a hard blocker. Both log on failure, never crash.
    let voice = if cli.dry_voice {
        warn!("Dry-voice mode: spoken lines + klaxon will be LOGGED, not sent (Overwatch not contacted)");
        Some(out::voice::VoiceClient::new_dry())
    } else {
        cfg.overwatch.as_ref().map(|o| {
            info!(
                host = %o.host, port = o.port,
                klaxon_enabled = o.klaxon_enabled,
                klaxon_volume = o.klaxon_volume, voice_volume = o.voice_volume,
                duck_factor = o.duck_factor,
                "Voice output enabled (Overwatch gRPC)"
            );
            out::voice::VoiceClient::new(
                &o.host,
                o.port,
                o.alarm_id.clone(),
                o.voice_id.clone(),
                o.klaxon_enabled,
                o.klaxon_volume,
                o.voice_volume,
                o.duck_factor,
            )
        })
    };
    let pagerduty = cfg.pagerduty.as_ref().and_then(|p| {
        if p.routing_key.trim().is_empty() {
            info!("PagerDuty configured but routing_key empty; PagerDuty output disabled");
            None
        } else {
            info!(source = %p.source, dedup_key = %p.dedup_key, "PagerDuty output enabled (Events v2)");
            Some(out::pagerduty::PagerDuty::new(
                p.routing_key.clone(),
                p.source.clone(),
                p.dedup_key.clone(),
            ))
        }
    });
    if voice.is_some() || pagerduty.is_some() {
        info!("Spawning Phase 3 outputs consumer (voice + PagerDuty)");
        tokio::spawn(out::run(outputs_rx, outputs_mode_rx, voice, pagerduty, cfg.offsite.clone()));
    }

    // Phase 5 control channel: the `/control` WS (on the Phase 4 HUD server)
    // forwards `acknowledge`/`standdown` commands from the HA component onto this
    // mpsc; the engine's `select!` loop consumes the receiver. Created
    // unconditionally so the engine always has a receiver; the sender is only
    // handed to the HUD server when `web:` is configured (no web = no inbound
    // control path, the receiver simply never fires). Argus always runs the web
    // server in production, so the control surface is always present there.
    let (control_tx, control_rx) = mpsc::channel::<ControlCommand>(16);

    // Phase 4 — kiosk HUD. The HUD HTTP/WS server spawns iff `web:` is configured;
    // the kiosk-takeover consumer spawns iff `web:` is configured AND `kiosks:` is
    // non-empty (the takeover URL is `web.public_base`, so no web = nothing to
    // navigate to). Both are daemon-only and never crash the daemon on failure.
    if let Some(web_cfg) = cfg.web.clone() {
        let case_base = case::default_case_base()?;
        info!(bind = %web_cfg.bind, "Spawning Phase 4 HUD server (+ Phase 5 /control WS)");
        let public_base = web_cfg.public_base.clone();
        tokio::spawn(web::serve(web_cfg, web_rx, web_mode_rx, case_base, control_tx.clone()));

        if !cfg.kiosks.is_empty() {
            // The takeover consumer drives HA directly, so it needs its own
            // RestClient (the engine owns the one built above).
            let kiosk_rest = RestClient::new(&cfg.ha.url, &cfg.ha.token)?;
            info!(kiosks = cfg.kiosks.len(), "Spawning Phase 4 kiosk-takeover consumer");
            tokio::spawn(out::kiosks::run(
                kiosks_rx,
                kiosks_mode_rx,
                cfg.kiosks.clone(),
                kiosk_rest,
                public_base,
            ));
        }
    }

    // Drop the original control sender; the web server (if spawned) holds a clone.
    // With no web server, all senders are gone, so the engine's control arm never
    // fires — harmless.
    drop(control_tx);

    run_daemon(cfg, eng, control_rx).await
}

/// Subscribe to the HA alarm and drive the engine until Ctrl-C.
async fn run_daemon(
    cfg: Config,
    eng: Engine,
    control_rx: mpsc::Receiver<ControlCommand>,
) -> Result<()> {
    let (tx, rx) = mpsc::channel::<HaEvent>(16);

    let ws_url = ha::websocket_url(&cfg.ha.url);
    let token = cfg.ha.token.clone();
    // The primary `Alarm`-profile panel drives the full arm/disarm state machine.
    // The softer (non-alarm) trigger sources (Phase 4b) each open a GATED case on
    // their active edge — typically empty (the live atlas config has only the
    // alarm entity; the perimeter/front-door entities are user-owned).
    let alarm_entity = cfg.primary_alarm_entity();
    let softer_triggers: std::collections::HashMap<String, case::TriggerProfile> = cfg
        .trigger_profile_map()
        .into_iter()
        .filter(|(e, _)| *e != alarm_entity)
        .collect();
    let standdown_entity = cfg.standdown_entity.clone();
    tokio::spawn(async move {
        ha::ws::run(ws_url, token, alarm_entity, softer_triggers, standdown_entity, tx).await;
    });

    info!(
        "Watching {} for trigger transitions; press Ctrl-C to stop",
        cfg.primary_alarm_entity()
    );

    // Run the engine; cancel on Ctrl-C.
    let engine_task = tokio::spawn(eng.run(rx, control_rx));
    tokio::signal::ctrl_c().await.ok();
    info!("Shutdown requested");
    engine_task.abort();
    Ok(())
}
