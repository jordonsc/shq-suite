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
//! broadcast (timeline diff → voice/PagerDuty) AND the `watch<AlarmMode>` broadcast
//! so it can run the **disarm** actions Argus took over from HA: on a disarm it
//! stops the klaxon out-of-band (not behind the speech queue), flushes any queued
//! intruder lines, and announces "Alarm standing down."

pub mod kiosks;
pub mod offsite;
pub mod pagerduty;
pub mod voice;
pub mod voice_policy;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::{mpsc, watch};
use tracing::{debug, info};

use crate::case::{CaseState, TimelineKind};
use crate::config::OffsiteConfig;
use crate::state::AlarmMode;
use pagerduty::PagerDuty;
use voice::VoiceClient;

/// A message to the serial voice worker. `Speak` carries the `generation` it was
/// queued in: a disarm bumps the generation, so any line still queued from before
/// the disarm is skipped (the queue is "flushed" without playing).
enum VoiceMsg {
    Speak { text: String, generation: u64 },
    Alarm(bool),
}

/// Spawned in daemon mode only. Subscribes to the case broadcast and the alarm-mode
/// broadcast, and drives the voice + PagerDuty outputs.
///
/// - `voice` / `pd` are `None` when the respective config is absent.
/// - `offsite` (clone of config) lets the PagerDuty dossier embed the deterministic
///   S3 case prefix without a runtime S3 call.
pub async fn run(
    mut rx: watch::Receiver<Option<CaseState>>,
    mut mode_rx: watch::Receiver<AlarmMode>,
    voice: Option<VoiceClient>,
    pd: Option<PagerDuty>,
    offsite: OffsiteConfig,
) {
    // Speech is serialised through one worker so lines don't overlap. A monotonic
    // generation lets a disarm flush queued-but-unplayed lines (the worker skips
    // any `Speak` whose generation is older than the current one).
    let voice_gen = Arc::new(AtomicU64::new(0));
    let speech_tx = voice
        .as_ref()
        .map(|vc| spawn_voice_worker(vc.clone(), voice_gen.clone()));

    // Per-case cursor: how many timeline events we have already routed.
    let mut cursors: HashMap<String, usize> = HashMap::new();
    // Track the alarm mode so we fire the disarm actions on the active→disarmed edge.
    let mut prev_mode = *mode_rx.borrow();

    info!(
        voice = speech_tx.is_some(),
        pagerduty = pd.is_some(),
        "outputs: Phase 3 consumer started"
    );

    loop {
        tokio::select! {
            // ── Case stream: route new timeline events ───────────────────────
            changed = rx.changed() => {
                if changed.is_err() {
                    debug!("outputs: case broadcast closed; exiting");
                    break;
                }
                let state = match rx.borrow_and_update().clone() {
                    Some(s) => s,
                    None => continue, // no active case
                };
                let cursor = cursors.entry(state.case_id.clone()).or_insert(0);
                let already = *cursor;
                let total = state.timeline.len();
                if total > already {
                    for idx in already..total {
                        route_event(&state.timeline[idx], &state, &offsite,
                                    speech_tx.as_ref(), &voice_gen, pd.as_ref()).await;
                    }
                    *cursor = total;
                }
            }
            // ── Alarm mode: disarm actions (klaxon off + flush + standdown) ──
            changed = mode_rx.changed() => {
                if changed.is_err() {
                    debug!("outputs: mode broadcast closed; exiting");
                    break;
                }
                let mode = *mode_rx.borrow_and_update();
                let was_active = matches!(
                    prev_mode,
                    AlarmMode::Arming | AlarmMode::Armed | AlarmMode::Triggered
                );
                let now_disarmed = matches!(mode, AlarmMode::Disarmed | AlarmMode::Authorised);
                prev_mode = mode;
                if was_active && now_disarmed {
                    standdown_voice(voice.as_ref(), speech_tx.as_ref(), &voice_gen).await;
                }
            }
        }
    }
}

/// Disarm handling Argus took over from HA: stop the klaxon immediately
/// (out-of-band, so it can't wait behind a blocking `Verbalise`), flush any queued
/// intruder lines (bump the generation), then announce the standdown.
async fn standdown_voice(
    voice: Option<&VoiceClient>,
    speech_tx: Option<&mpsc::Sender<VoiceMsg>>,
    voice_gen: &AtomicU64,
) {
    // 1. Klaxon off — direct, not via the serial worker (which may be mid-clip).
    if let Some(v) = voice {
        v.set_alarm(false).await;
    }
    // 2. Flush queued speech: anything queued before now is skipped by the worker.
    let generation = voice_gen.fetch_add(1, Ordering::SeqCst) + 1;
    // 3. Announce the standdown (tagged with the fresh generation so it survives).
    if let Some(tx) = speech_tx {
        let _ = tx.try_send(VoiceMsg::Speak {
            text: "Alarm standing down.".to_string(),
            generation,
        });
        info!("outputs: disarm — klaxon off, queue flushed, standdown announced");
    }
}

/// Route ONE new timeline event to the voice gate + the PagerDuty updater. The two
/// are independent: a PD failure cannot stop speech and vice-versa (each logs).
async fn route_event(
    event: &crate::case::TimelineEvent,
    state: &CaseState,
    offsite: &OffsiteConfig,
    speech_tx: Option<&mpsc::Sender<VoiceMsg>>,
    voice_gen: &AtomicU64,
    pd: Option<&PagerDuty>,
) {
    // ── Output gate (Phase 4a, DORMANT) ──────────────────────────────────────
    // A GATED case (a softer `Investigate`/`General` posture) suppresses ALL
    // outward channels — voice, klaxon, PagerDuty — until a Phase-4b promotion
    // raises its `effective_profile` to `Alarm`. The `Escalated` milestone is the
    // promotion edge itself, so it is always let through (4b speaks the breach
    // line / fires the first page off it). No gated case opens today (every
    // trigger is the alarm → `Alarm`), so this is inert in 4a — but it preserves
    // the invariant the moment the softer triggers are wired.
    if state.gated() && event.kind != TimelineKind::Escalated {
        return;
    }

    // ── Voice channel: gate → queued speech (zero or more lines) ─────────────
    if let Some(tx) = speech_tx {
        let generation = voice_gen.load(Ordering::SeqCst);
        for line in voice_policy::lines_for(event, state) {
            // Bounded, non-blocking: if the queue is somehow full we drop rather
            // than stall the loop (speech is best-effort intimidation, not record).
            if tx
                .try_send(VoiceMsg::Speak { text: line.text, generation })
                .is_err()
            {
                debug!("outputs: voice queue full; dropped a line");
            }
        }
        // Klaxon ON rides the case open. Klaxon OFF is handled out-of-band by the
        // disarm path (`standdown_voice`), NOT here, so a stop can't wait behind a
        // playing line.
        if event.kind == TimelineKind::CaseOpened {
            let _ = tx.try_send(VoiceMsg::Alarm(true));
        }
    }

    // ── PagerDuty channel — the full incident lifecycle (stable dedup_key=case_id):
    //   trigger (CaseOpened) → update (re-trigger on a material change, same
    //   dedup_key, so the open incident reflects the latest dossier) → acknowledge
    //   (operator ack via the Phase-5 control WS) → resolve (standdown / cleared).
    if let Some(pd) = pd {
        match event.kind {
            // Material changes → (re)send a trigger with the latest dossier.
            TimelineKind::CaseOpened
            | TimelineKind::IntruderDetected
            | TimelineKind::IntruderIdentified
            | TimelineKind::WeaponDetected
            | TimelineKind::ThreatLevelChanged => {
                pd.trigger(state, Some(offsite)).await;
            }
            // Operator acknowledged → move the open incident to ACKNOWLEDGED.
            TimelineKind::Acknowledged => {
                pd.acknowledge(state).await;
            }
            TimelineKind::Standdown | TimelineKind::Cleared => {
                pd.resolve(state).await;
            }
            // BestStillUpgraded / SecurityStationNotified don't re-page on their own.
            _ => {}
        }
    }
}

/// Spawn the single voice worker. It owns the `VoiceClient` and processes the queue
/// serially (the pacing guarantee — no overlapping `Verbalise`). A `Speak` whose
/// `generation` is older than the current one is SKIPPED — that's how a disarm
/// flushes intruder lines still sitting in the queue.
fn spawn_voice_worker(client: VoiceClient, voice_gen: Arc<AtomicU64>) -> mpsc::Sender<VoiceMsg> {
    let (tx, mut rx) = mpsc::channel::<VoiceMsg>(32);
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            match msg {
                VoiceMsg::Alarm(on) => client.set_alarm(on).await,
                VoiceMsg::Speak { text, generation } => {
                    // Skip lines flushed by a disarm (queued before the dwell bumped
                    // the generation). The currently-playing clip can't be stopped
                    // mid-flight, but everything queued behind it is dropped.
                    if generation < voice_gen.load(Ordering::SeqCst) {
                        continue;
                    }
                    // `verbalise` requests blocking playback (`await_playback=true`),
                    // so this RPC returns only after Overwatch finishes PLAYING the
                    // clip — the serial worker paces itself, no timing estimate.
                    client.verbalise(&text).await;
                }
            }
        }
        debug!("outputs: voice worker stopped");
    });
    tx
}
