//! Output sinks that consume the Phase 2 case stream + case dir.
//!
//! These render/replicate the [`crate::case::CaseState`] — none of them call the
//! LLM or HA. The sinks:
//! - **`offsite`** (Phase 2a): a real-time S3 mirror of the on-disk case dir.
//! - **`voice`** (Phase 3): the Overwatch gRPC klaxon + positive-only speech.
//! - **`voice_policy`** (Phase 3): the pure positive-only gate (timeline → line).
//! - **`pagerduty`** (Phase 3): the security-station dossier (Events v2).
//!
//! [`run`] is the Phase 3 wiring: it subscribes to the `watch<Option<CaseState>>`
//! broadcast, **diffs the timeline** across updates (a per-case cursor on timeline
//! length), and routes each NEW [`crate::case::TimelineEvent`] to two INDEPENDENT
//! channels — voice and PagerDuty. A failure in one never blocks the other (each
//! logs and continues). Speech is serialised through a single voice worker so
//! milestones don't talk over each other.

pub mod offsite;
pub mod pagerduty;
pub mod voice;
pub mod voice_policy;

use std::collections::HashMap;

use tokio::sync::{mpsc, watch};
use tracing::{debug, info};

use crate::case::{CaseState, TimelineKind};
use crate::config::OffsiteConfig;
use pagerduty::PagerDuty;
use voice::VoiceClient;

/// Spawned in daemon mode only. Subscribes to the case broadcast and drives the
/// voice + PagerDuty outputs off new timeline events.
///
/// - `voice` / `pd` are `None` when the respective config is absent (the channel
///   is then simply not driven).
/// - `offsite` is passed (clone of config) so the PagerDuty dossier can embed the
///   deterministic S3 case prefix without any runtime S3 call.
///
/// On a successful PagerDuty `trigger` for a **material** event, we speak the
/// `security_station_notified` line DIRECTLY (M1): Phase 3 cannot mutate the
/// engine-owned `CaseState`, so we can't push a `SecurityStationNotified`
/// timeline event for the gate to pick up. Surfacing it in the HUD ticker (via a
/// feedback channel back to the engine) is a noted future.
pub async fn run(
    mut rx: watch::Receiver<Option<CaseState>>,
    voice: Option<VoiceClient>,
    pd: Option<PagerDuty>,
    offsite: OffsiteConfig,
) {
    // Serialise speech through one worker so lines don't overlap. The gate is
    // pure; pacing/queueing lives here. A small bounded queue drops nothing under
    // normal milestone rates (a handful of events per case).
    let speech_tx = voice.as_ref().map(|vc| spawn_voice_worker(vc.clone()));

    // Per-case cursor: how many timeline events we have already routed. Diffing
    // against `timeline.len()` yields the new tail each update.
    let mut cursors: HashMap<String, usize> = HashMap::new();

    info!(
        voice = speech_tx.is_some(),
        pagerduty = pd.is_some(),
        "outputs: Phase 3 consumer started"
    );

    loop {
        if rx.changed().await.is_err() {
            debug!("outputs: broadcast closed; exiting");
            break;
        }
        // Clone out of the watch guard quickly so we don't hold the lock across awaits.
        let state = match rx.borrow_and_update().clone() {
            Some(s) => s,
            None => continue, // no active case
        };

        let cursor = cursors.entry(state.case_id.clone()).or_insert(0);
        let already = *cursor;
        let total = state.timeline.len();
        if total <= already {
            // No new timeline events (e.g. a tick that only refreshed locations).
            continue;
        }

        // Route each NEW event through both channels.
        for idx in already..total {
            let event = &state.timeline[idx];
            route_event(event, &state, &offsite, speech_tx.as_ref(), pd.as_ref()).await;
        }
        *cursor = total;
    }
}

/// Route ONE new timeline event to the voice gate + the PagerDuty updater. The two
/// are independent: a PD failure cannot stop speech and vice-versa (each logs).
async fn route_event(
    event: &crate::case::TimelineEvent,
    state: &CaseState,
    offsite: &OffsiteConfig,
    speech_tx: Option<&mpsc::Sender<String>>,
    pd: Option<&PagerDuty>,
) {
    // ── Voice channel: positive-only gate → queued speech ───────────────────
    if let Some(tx) = speech_tx {
        if let Some(line) = voice_policy::line_for(event, state) {
            // Bounded, non-blocking: if the queue is somehow full we drop rather
            // than stall the loop (speech is best-effort intimidation, not record).
            if tx.try_send(line.text).is_err() {
                debug!("outputs: voice queue full; dropped a line");
            }
        }
        // The klaxon is bound to case lifecycle, not a spoken line:
        match event.kind {
            TimelineKind::CaseOpened => queue_alarm(tx, true),
            TimelineKind::Standdown | TimelineKind::Cleared => queue_alarm(tx, false),
            _ => {}
        }
    }

    // ── PagerDuty channel: trigger on material change, resolve on standdown ──
    if let Some(pd) = pd {
        match event.kind {
            // Material changes → (re)send a trigger with the latest dossier.
            TimelineKind::CaseOpened
            | TimelineKind::IntruderDetected
            | TimelineKind::IntruderIdentified
            | TimelineKind::ThreatLevelChanged => {
                pd.trigger(state, Some(offsite)).await;
                // M1: speak the security-station line directly on a successful-ish
                // path. We don't get a hard ack from `send` (it logs internally),
                // so for the open event we announce notification. Bind it to
                // CaseOpened only to avoid repeating the line on every update.
                if event.kind == TimelineKind::CaseOpened {
                    if let Some(tx) = speech_tx {
                        let _ = tx.try_send(
                            "Security station has received your image and location.".to_string(),
                        );
                    }
                }
            }
            TimelineKind::Standdown | TimelineKind::Cleared => {
                pd.resolve(state).await;
            }
            // BestStillUpgraded / SecurityStationNotified don't re-page on their own.
            _ => {}
        }
    }
}

/// Queue a klaxon control on the same serial channel as speech (so a stop doesn't
/// race a spoken line). We tunnel it as a sentinel string the worker recognises.
fn queue_alarm(tx: &mpsc::Sender<String>, on: bool) {
    let cmd = if on { ALARM_ON } else { ALARM_OFF };
    let _ = tx.try_send(cmd.to_string());
}

const ALARM_ON: &str = "\u{0}alarm:on";
const ALARM_OFF: &str = "\u{0}alarm:off";

/// Spawn the single voice worker. It owns the `VoiceClient` and processes the
/// queue serially: alarm sentinels drive `SetAlarm`, everything else is spoken.
/// Serial processing is the pacing guarantee (no overlapping `Verbalise`).
fn spawn_voice_worker(client: VoiceClient) -> mpsc::Sender<String> {
    let (tx, mut rx) = mpsc::channel::<String>(32);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg.as_str() {
                ALARM_ON => client.set_alarm(true).await,
                ALARM_OFF => client.set_alarm(false).await,
                line => client.verbalise(line).await,
            }
        }
        debug!("outputs: voice worker stopped");
    });
    tx
}
