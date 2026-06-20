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
    Speak {
        text: String,
        /// Per-line volume override (the Investigate loop's 0.7 announce / stand-
        /// down lines); `None` = the configured `voice_volume`.
        volume: Option<f32>,
        generation: u64,
    },
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
                                    voice.as_ref(), speech_tx.as_ref(), &voice_gen,
                                    pd.as_ref()).await;
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
            volume: None,
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
    voice: Option<&VoiceClient>,
    speech_tx: Option<&mpsc::Sender<VoiceMsg>>,
    voice_gen: &AtomicU64,
    pd: Option<&PagerDuty>,
) {
    // ── Gate split (0.32.0) ──────────────────────────────────────────────────
    // VOICE is NO LONGER gated here — it always runs through `voice_policy`, which
    // is itself profile-aware: it returns the Investigate smart-alarm announce /
    // outcome / stand-down lines for a gated Investigate case, and stays EMPTY for
    // a gated General case (silent until escalated). The HARD outputs (klaxon ON,
    // PagerDuty) stay gated — they only fire on a non-gated case OR on the
    // `Escalated` promotion edge itself. So: Investigate speaks but doesn't klaxon/
    // page until it escalates; General is fully silent until escalate; Alarm (never
    // gated) is unchanged. The kiosks consumer + HUD/journal gating live elsewhere.
    let hard_outputs_allowed = !state.gated() || event.kind == TimelineKind::Escalated;

    // ── Voice channel: profile-aware gate → queued speech (zero or more lines) ─
    if let Some(tx) = speech_tx {
        let generation = voice_gen.load(Ordering::SeqCst);
        for line in voice_policy::lines_for(event, state) {
            // Bounded, non-blocking: if the queue is somehow full we drop rather
            // than stall the loop (speech is best-effort intimidation, not record).
            if tx
                .try_send(VoiceMsg::Speak {
                    text: line.text,
                    volume: line.volume,
                    generation,
                })
                .is_err()
            {
                debug!("outputs: voice queue full; dropped a line");
            }
        }
        // Klaxon ON rides the case open — or the `Escalated` promotion of a gated
        // case (Phase 4b), which is the first justified moment to sound it. It is a
        // HARD output: gated cases (Investigate announce / General) must NOT sound
        // the siren until escalated. Klaxon OFF is handled out-of-band by the disarm
        // path (`standdown_voice`), NOT here, so a stop can't wait behind a line.
        if hard_outputs_allowed
            && matches!(event.kind, TimelineKind::CaseOpened | TimelineKind::Escalated)
        {
            let _ = tx.try_send(VoiceMsg::Alarm(true));
        }
    }

    // ── Klaxon OFF on ANY case-clear (0.32.0) ────────────────────────────────
    // The mode-edge `standdown_voice` only fires on the alarm-mode active→disarmed
    // edge. A control/deadline/kill-switch standdown produces a `Standdown`/`Cleared`
    // timeline event with NO mode edge — so without this the klaxon would keep
    // sounding. Stop it OUT-OF-BAND here (direct `set_alarm(false)`, the same call
    // `standdown_voice` uses), independently of the PagerDuty resolve below. A
    // redundant `SetAlarm(false)` is harmless (idempotent), so this is safe even
    // when the mode-edge path also fires. Placed BEFORE the gated early-return so it
    // covers a (rare) gated case that had somehow sounded the siren.
    if matches!(event.kind, TimelineKind::Standdown | TimelineKind::Cleared) {
        if let Some(v) = voice {
            v.set_alarm(false).await;
        }
    }

    // The hard outputs below (PagerDuty) are suppressed for a gated case.
    if !hard_outputs_allowed {
        return;
    }

    // ── PagerDuty channel — the full incident lifecycle (stable dedup_key=case_id):
    //   trigger (CaseOpened) → update (re-trigger on a material change, same
    //   dedup_key, so the open incident reflects the latest dossier) → acknowledge
    //   (operator ack via the Phase-5 control WS) → resolve (standdown / cleared).
    if let Some(pd) = pd {
        match event.kind {
            // Material changes → (re)send a trigger with the latest dossier.
            // `Escalated` (Phase 4b) is a case-open-equivalent: it's the first page
            // for a promoted gated case (the gated `CaseOpened` was suppressed).
            TimelineKind::CaseOpened
            | TimelineKind::Escalated
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
                VoiceMsg::Speak { text, volume, generation } => {
                    // Skip lines flushed by a disarm (queued before the dwell bumped
                    // the generation). The currently-playing clip can't be stopped
                    // mid-flight, but everything queued behind it is dropped.
                    if generation < voice_gen.load(Ordering::SeqCst) {
                        continue;
                    }
                    // `verbalise` requests blocking playback (`await_playback=true`),
                    // so this RPC returns only after Overwatch finishes PLAYING the
                    // clip — the serial worker paces itself, no timing estimate.
                    client.verbalise(&text, volume).await;
                }
            }
        }
        debug!("outputs: voice worker stopped");
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{CaseState, TimelineEvent, TriggerProfile};
    use chrono::Utc;

    /// Drain everything currently queued on a voice channel, classifying each
    /// message: returns `(spoken_lines, klaxon_on_count)`.
    fn drain(rx: &mut mpsc::Receiver<VoiceMsg>) -> (Vec<(String, Option<f32>)>, usize) {
        let mut spoken = Vec::new();
        let mut klaxon = 0;
        while let Ok(msg) = rx.try_recv() {
            match msg {
                VoiceMsg::Speak { text, volume, .. } => spoken.push((text, volume)),
                VoiceMsg::Alarm(true) => klaxon += 1,
                VoiceMsg::Alarm(false) => {}
            }
        }
        (spoken, klaxon)
    }

    fn opened() -> TimelineEvent {
        TimelineEvent {
            at: Utc::now(),
            kind: TimelineKind::CaseOpened,
            detail: "opened".to_string(),
        }
    }

    /// THE GATE SPLIT (c): a gated `Investigate` case routes its 0.7 announce VOICE
    /// line on `CaseOpened`, but NO klaxon (a hard output) — and `route_event`
    /// returns before any PagerDuty (passed `None` here; the early-return is the
    /// proof the PD block is never reached).
    #[tokio::test]
    async fn gated_investigate_speaks_but_no_klaxon_or_pd() {
        let (tx, mut rx) = mpsc::channel::<VoiceMsg>(8);
        let voice_gen = AtomicU64::new(0);
        let offsite = OffsiteConfig::default();

        let mut state = CaseState::new("case-inv".to_string(), Utc::now(), TriggerProfile::Investigate);
        state.trigger_location = Some("Backyard".to_string());
        let ev = opened();

        route_event(&ev, &state, &offsite, None, Some(&tx), &voice_gen, None).await;

        let (spoken, klaxon) = drain(&mut rx);
        assert_eq!(spoken.len(), 1, "the investigate announce must be spoken");
        assert_eq!(spoken[0].0, "Security alert, Backyard, investigating.");
        assert_eq!(spoken[0].1, Some(0.7), "spoken at 0.7, not full volume");
        assert_eq!(klaxon, 0, "a gated investigate case must NOT sound the klaxon");
    }

    /// THE GATE SPLIT (c): a gated `General` case routes NOTHING on `CaseOpened` —
    /// no voice line (the policy is silent for gated General) and no klaxon.
    #[tokio::test]
    async fn gated_general_routes_nothing() {
        let (tx, mut rx) = mpsc::channel::<VoiceMsg>(8);
        let voice_gen = AtomicU64::new(0);
        let offsite = OffsiteConfig::default();

        let state = CaseState::new("case-gen".to_string(), Utc::now(), TriggerProfile::General);
        let ev = opened();

        route_event(&ev, &state, &offsite, None, Some(&tx), &voice_gen, None).await;

        let (spoken, klaxon) = drain(&mut rx);
        assert!(spoken.is_empty(), "a gated General case is fully silent on open");
        assert_eq!(klaxon, 0, "no klaxon for a gated General case");
    }

    /// A real (non-gated) `Alarm` case is UNCHANGED: the breach line + the klaxon
    /// both fire on `CaseOpened`, at the default (full) volume.
    #[tokio::test]
    async fn alarm_case_speaks_breach_and_klaxon() {
        let (tx, mut rx) = mpsc::channel::<VoiceMsg>(8);
        let voice_gen = AtomicU64::new(0);
        let offsite = OffsiteConfig::default();

        let mut state = CaseState::new("case-alarm".to_string(), Utc::now(), TriggerProfile::Alarm);
        state.trigger_location = Some("Garage".to_string());
        let ev = opened();

        route_event(&ev, &state, &offsite, None, Some(&tx), &voice_gen, None).await;

        let (spoken, klaxon) = drain(&mut rx);
        assert_eq!(spoken.len(), 1);
        assert_eq!(spoken[0].0, "Security breach detected in Garage.");
        assert_eq!(spoken[0].1, None, "alarm breach line uses the default volume");
        assert_eq!(klaxon, 1, "the alarm klaxon sounds on open");
    }

    fn event(kind: TimelineKind) -> TimelineEvent {
        TimelineEvent {
            at: Utc::now(),
            kind,
            detail: String::new(),
        }
    }

    /// Klaxon-off on ANY case-clear (0.32.0): a `Cleared`/`Standdown` timeline event
    /// — with NO alarm-mode edge (e.g. a control/deadline/kill-switch standdown) —
    /// must hit the out-of-band `set_alarm(false)` path. The voice gate emits no
    /// SPEAK line for these kinds (the policy returns `None`), so the proof the
    /// klaxon-off branch ran is that `route_event` drives a (dry) `VoiceClient`
    /// without panicking and queues no speech. A dry client logs the SetAlarm rather
    /// than dialling, so the test needs no network. Asserts both `Cleared` and
    /// `Standdown` reach it.
    #[tokio::test]
    async fn case_clear_stops_klaxon_out_of_band() {
        let voice = VoiceClient::new_dry();
        let voice_gen = AtomicU64::new(0);
        let offsite = OffsiteConfig::default();
        let state = CaseState::new("case-clear".to_string(), Utc::now(), TriggerProfile::Alarm);

        for kind in [TimelineKind::Standdown, TimelineKind::Cleared] {
            let (tx, mut rx) = mpsc::channel::<VoiceMsg>(8);
            let ev = event(kind);
            // No PD passed: the klaxon-off runs BEFORE the (None) PD block, so this
            // exercises the new branch directly. Must not panic.
            route_event(&ev, &state, &offsite, Some(&voice), Some(&tx), &voice_gen, None).await;
            let (spoken, klaxon) = drain(&mut rx);
            assert!(spoken.is_empty(), "{kind:?}: no SPEAK line on a clear");
            assert_eq!(klaxon, 0, "{kind:?}: the OFF is out-of-band, not a VoiceMsg::Alarm(true)");
        }
    }
}
