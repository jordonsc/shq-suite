//! The assessment engine — the Phase 2 brain.
//!
//! While the HA alarm is `triggered`, every `cadence_secs` the engine captures
//! all cameras + telemetry, runs the **Sonnet** live what/where loop (structured
//! output), merges the delta into the evolving [`CaseState`], persists best
//! stills, and escalates new/improved intruders to the **Opus** forensic pass.
//! Every mutation is journalled to the case dir (the Phase 2a upload queue) and
//! broadcast over a `watch` channel for Phases 3/4.
//!
//! Argus — not the LLM — derives the [`TimelineKind`] milestones by diffing
//! state, giving Phase 3 a controlled vocabulary.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::case::{
    default_case_base, identification_schema, live_assessment_schema, CaseDir, CaseState,
    CaseStatus, Identification, Intruder, IntruderDelta, LiveAssessment, LocationDelta,
    LocationObservation, StillRef, ThreatLevel, TimelineKind, TriggerProfile,
};
use crate::config::{Config, ResidentPhoto};
use crate::ha::{HaEvent, RestClient};
use crate::llm::{AnthropicClient, AssessRequest, ImageInput, Reasoning, Usage};
use crate::state::AlarmMode;

/// A command from the Phase 5 control surface (the HA component over the
/// `/control` WS) into the engine. Delivered over a dedicated mpsc the engine's
/// `select!` multiplexes alongside the HA event stream.
#[derive(Debug, Clone, Copy)]
pub enum ControlCommand {
    /// Operator-initiated standdown — same path as the HA `AlarmCleared` edge.
    Standdown,
    /// Operator acknowledged the active case (records a timeline milestone; the
    /// case keeps assessing).
    Acknowledge,
}

/// Cap on stored best stills per intruder.
const MAX_BEST_STILLS: usize = 5;
/// Frame-quality (`id_quality`) improvement required before grabbing a fresh best
/// still + re-running the Opus forensic pass on a clearer shot.
const QUALITY_IMPROVE: f32 = 0.12;
/// The forensic identification is SPOKEN (and re-paged) once an intruder's
/// confidence first reaches this — so the announced profile is a solid one, not a
/// guess off a first-glimpse frame. Below it, the forensic pass still firms up the
/// record/dossier silently. Validated against live runs: a real intruder sits
/// ~0.9+, while a distant passer-by / false figure sits ~0.3, so 0.5 cleanly
/// separates the two (a lower bar risks announcing a vague profile for the latter).
const ID_SPEAK_CONF: f32 = 0.5;
/// Sonnet live-loop output budget (compact JSON).
const LIVE_MAX_TOKENS: u32 = 1500;
/// Opus forensic output budget (descriptors + dossier + adaptive thinking).
const ID_MAX_TOKENS: u32 = 4096;

/// Threat NEVER downgrades from the model during an active incident — only these
/// inactivity timers lower it (and only manual disarm / the auto-close timeout
/// ends the case). Seconds since the last person was seen on any camera:
/// - ≥ this → cap the threat at `Elevated`.
const DECAY_ELEVATED_SECS: i64 = 180;
/// - ≥ this → cap the threat at `Info` (low).
const DECAY_LOW_SECS: i64 = 600;
/// - ≥ this → auto-close the Argus case (1 hour of no activity).
const AUTO_CLOSE_SECS: i64 = 3600;

/// Hold the green AUTHORISED state on the HUD this long after a disarm before
/// reverting to standby / dashboard (both the post-incident and no-incident
/// disarm paths use it).
const AUTHORISED_DWELL_SECS: u64 = 15;

/// `General` (benign-doorstep) standdown budget: a gated `General` case that sees
/// nothing of concern auto-closes quietly after this many ticks. Kept small —
/// `General` is a fast, shallow glance for OBVIOUS danger, not a deep ID; it must
/// stay cheap (it fires on every doorstep visitor) and silent.
const GENERAL_STANDDOWN_TICKS: u64 = 4;

/// `Investigate` (perimeter-security) silent-run budget: a gated `Investigate`
/// case quietly stands down when this window elapses with no escalation. Used to
/// seed `ActiveCase.investigate_deadline` at case open (a real perimeter-cooldown
/// telemetry timer could replace it later; the default holds until then).
const INVESTIGATE_DEADLINE_SECS: u64 = 120;

/// What the authorised-dwell timer does when it elapses.
#[derive(Debug, Clone, Copy)]
enum Revert {
    /// Post-incident: drop the cleared case off the broadcast (HUD → standby).
    ClearCase,
    /// No-incident disarm: revert the transient `Authorised` mode to `Disarmed`.
    ToDisarmed,
}

/// The forensic (Opus) instruction — shared by the inline (`--once`) and the
/// spawned (daemon) identification paths.
const IDENTIFY_INSTRUCTION: &str = "The LAST image is the clearest still of a detected subject \
    (the camera frame, labelled \"Camera: ...\"). Any images labelled \"Resident reference\" are \
    photos of KNOWN AUTHORISED RESIDENTS — compare the subject in the camera still against them. \
    If the subject matches a resident reference photo BEYOND REASONABLE DOUBT, identify the person \
    as that resident: say so in the descriptors/dossier, set `confidence` to reflect the match, and \
    do NOT treat them as an intruder. A mere resemblance (similar build, hair, skin tone, or \
    clothing) is NOT a match — when the match is uncertain, or the subject matches no reference \
    photo, treat them as an intruder. Fail toward intruder. (If no \"Resident reference\" images are \
    present, simply identify the subject as an unknown intruder.)\n\n\
    Produce a firm forensic identification of the subject in the camera still: physical \
    descriptors, distinguishing features, and a short dossier paragraph for the security station. \
    ALSO write `spoken_summary` — a single short sentence (max ~18 words) suitable to be read aloud \
    over a security speaker (sex, approx age, build, key clothing, one standout feature; no \
    preamble). Also determine whether the person is armed — examine the hands and any held object \
    CLOSELY (this is the clearest frame, your best chance to confirm or rule out a weapon the live \
    pass was unsure about). Set `armed` true if they hold a weapon or any object plausibly \
    consistent with one (blade, firearm, bat, stick, tool, or an elongated/pointed/metallic object) \
    and name it in `weapon`, hedging if unsure. Anchor every claim on the images.";

/// The result of a forensic (Opus) identification, delivered back to the engine's
/// event loop over an mpsc so the Opus call runs CONCURRENTLY with the live loop
/// instead of blocking the tick. Carries the case + intruder it belongs to so a
/// late result from a since-cleared case is dropped.
struct IdResult {
    case_id: String,
    intruder_id: String,
    ident: Result<Identification>,
    usage: Option<Usage>,
}

/// Run one forensic identification call. Free function (no `&self`) so it can run
/// inline OR inside a spawned task with cloned client + seed.
///
/// `residents` are the labelled resident reference photos (Opus-only) — PREPENDED
/// to the request images (each as `"Resident reference: <name>"`) BEFORE the
/// suspect still so the model can anchor a confident resident match. An empty
/// slice leaves the request unchanged (the suspect still is the only image).
async fn run_identify(
    opus: &AnthropicClient,
    seed: &str,
    residents: &[ResidentPhoto],
    label: &str,
    jpeg: &[u8],
) -> (Result<Identification>, Option<Usage>) {
    let mut images: Vec<ImageInput> = residents
        .iter()
        .map(|r| ImageInput::resident(&r.name, &r.jpeg))
        .collect();
    images.push(ImageInput::camera(label, jpeg));
    let req = AssessRequest {
        seed,
        images,
        instruction: IDENTIFY_INSTRUCTION.to_string(),
        schema: Some(identification_schema()),
        max_tokens: ID_MAX_TOKENS,
        reasoning: Reasoning::Deep,
    };
    match opus.assess(&req).await {
        Ok(c) => {
            let usage = c.usage.clone();
            (c.parse(), Some(usage))
        }
        Err(e) => (Err(e), None),
    }
}

/// The live, mutating state of one alarm episode.
struct ActiveCase {
    state: CaseState,
    dir: CaseDir,
    /// Next `subject-N` index.
    next_subject: u32,
    /// Next still file index.
    next_still: u64,
    /// Best frame-quality (`id_quality`) at which each intruder's forensic still
    /// was last grabbed — so we only re-profile when a meaningfully CLEARER shot
    /// appears (not on a generic-confidence wobble).
    still_quality: HashMap<String, f32>,
    /// Intruder ids whose identification has already been SPOKEN (+ re-paged) —
    /// the announced profile fires once, on the first sufficiently-confident
    /// forensic result, not on every re-run.
    id_spoken: HashSet<String>,
    /// Intruder ids with an Opus forensic pass currently IN FLIGHT (spawned, not
    /// yet returned) — prevents queueing a second concurrent identify for the same
    /// person while one is running.
    identifying: HashSet<String>,
    /// Intruder ids for which a weapon-capture still has already been grabbed, so
    /// a persistently-armed person doesn't re-capture every tick.
    weapon_still: HashSet<String>,
    /// When a person was last seen on any camera. Drives the threat-decay timers
    /// and the auto-close timeout. Seeded to the case start so an empty case still
    /// decays + auto-closes.
    last_activity: DateTime<Utc>,
    /// Ticks attempted so far for this case (drives the full-sweep cadence: tick
    /// 0 is the baseline full sweep, then `full_sweep_every` re-sweeps).
    tick_count: u64,
    /// Zones (room/area names, lower-cased keys) already announced as "Intruder in
    /// <zone>." this case — so each zone is announced at most once. Seeded with the
    /// alarm's trigger zone at case open so the initial location is excluded.
    announced_zones: HashSet<String>,
    /// Whether the "initial" zone (the one excluded from movement announcements)
    /// has been pinned. True from case open when a trigger location was known;
    /// otherwise the FIRST zone an intruder is seen in becomes the initial one
    /// (suppressed), and subsequent fresh zones are announced.
    initial_zone_recorded: bool,
    /// Whether this case has been PROMOTED to `Alarm` (Phase 4b). Guards `promote`
    /// so it is idempotent — the real-alarm trip + the `Escalated` milestone fire
    /// at most once. An `Alarm`-profile case opens with this already `true` (it is
    /// born at full posture, never "promoted").
    escalated: bool,
    /// For an `Investigate` case: the instant its silent assessment window closes.
    /// When it passes with no escalation, the case quietly stands down. `None` for
    /// the other profiles (which don't use a deadline).
    investigate_deadline: Option<Instant>,
}

/// Running daily token usage for the optional cap.
struct DailyUsage {
    date: NaiveDate,
    input_tokens: u64,
}

/// The assessment engine. Owns the model clients, the seed, the camera map, and
/// the active case; consumes HA alarm transitions and broadcasts `CaseState`.
pub struct Engine {
    cfg: Config,
    rest: RestClient,
    sonnet: AnthropicClient,
    opus: AnthropicClient,
    seed: String,
    /// Labelled resident reference photos, loaded into memory at startup. Attached
    /// to the **Opus forensic ID** call ONLY (never the Sonnet live loop), prepended
    /// to the suspect still so the model can anchor a confident resident match.
    /// Empty (no `resident_photos:`, or none loaded) ⇒ the Opus call is unchanged.
    resident_photos: Vec<ResidentPhoto>,
    /// Camera label → entity id, for mapping the model's `*_label` fields back.
    label_to_entity: HashMap<String, String>,
    /// Entity id → camera label. The model sometimes returns the entity id (it's
    /// in the seed's camera map) where a label is expected; this normalises it
    /// back to the label so still-lookups (keyed by label) match.
    entity_to_label: HashMap<String, String>,
    /// Camera label → zone (room/area) name, for intruder-movement announcements.
    /// Defaults to the label itself when a camera has no explicit `zone`; the two
    /// outdoor-living cameras (and any other shared view) map to one zone.
    label_to_zone: HashMap<String, String>,
    state_tx: watch::Sender<Option<CaseState>>,
    active: Option<ActiveCase>,
    daily: DailyUsage,
    /// Set at the top of `run()`: the channel spawned Opus tasks post results to.
    /// `None` in `--once` (the forensic pass runs inline so the printed CaseState
    /// is complete).
    id_tx: Option<mpsc::Sender<IdResult>>,
    /// Wall-clock of the last assessment tick (the per-camera LLM calls). The loop
    /// targets a fixed FRAME PERIOD (`cadence_secs`) by waiting only the remainder
    /// not already consumed by this — so a slow tick fires the next one promptly,
    /// and only a fast tick gets padded.
    last_tick_duration: Duration,
    /// Broadcasts the coarse alarm mode (disarmed/arming/armed/triggered) for the
    /// kiosk HUD + takeover — even when there is no case. `None` in `--once`.
    mode_tx: Option<watch::Sender<AlarmMode>>,
    /// The last real alarm mode seen (from `from_ha`), to detect a disarm that
    /// came from an armed/arming/triggered state (→ show AUTHORISED) vs a
    /// startup/idle disarmed (→ nothing).
    prev_mode: Option<AlarmMode>,
    /// When set, the green AUTHORISED state is being held on the HUD; at this
    /// instant the dwell elapses and the engine reverts per [`Revert`]. Cancelled
    /// by any fresh alarm activity (new case / arming / armed).
    revert_pending: Option<(tokio::time::Instant, Revert)>,
}

impl Engine {
    pub fn new(
        cfg: Config,
        rest: RestClient,
        sonnet: AnthropicClient,
        opus: AnthropicClient,
        seed: String,
        resident_photos: Vec<ResidentPhoto>,
        state_tx: watch::Sender<Option<CaseState>>,
    ) -> Self {
        let label_to_entity: HashMap<String, String> = cfg
            .cameras
            .iter()
            .map(|c| (c.label.clone(), c.entity.clone()))
            .collect();
        let entity_to_label: HashMap<String, String> = cfg
            .cameras
            .iter()
            .map(|c| (c.entity.clone(), c.label.clone()))
            .collect();
        let label_to_zone: HashMap<String, String> = cfg
            .cameras
            .iter()
            .map(|c| {
                let zone = c
                    .zone
                    .as_deref()
                    .map(str::trim)
                    .filter(|z| !z.is_empty())
                    .unwrap_or(&c.label)
                    .to_string();
                (c.label.clone(), zone)
            })
            .collect();
        Self {
            cfg,
            rest,
            sonnet,
            opus,
            seed,
            resident_photos,
            label_to_entity,
            entity_to_label,
            label_to_zone,
            state_tx,
            active: None,
            daily: DailyUsage {
                date: Utc::now().date_naive(),
                input_tokens: 0,
            },
            id_tx: None,
            last_tick_duration: Duration::ZERO,
            mode_tx: None,
            prev_mode: None,
            revert_pending: None,
        }
    }

    /// Wire the alarm-mode broadcast channel (daemon only). The HUD + kiosk
    /// takeover consume it so arming/armed shows the standby pane even with no
    /// active case.
    pub fn set_mode_tx(&mut self, mode_tx: watch::Sender<AlarmMode>) {
        self.mode_tx = Some(mode_tx);
    }

    /// Broadcast the current alarm mode to the HUD/takeover (no-op in `--once`).
    fn broadcast_mode(&self, mode: AlarmMode) {
        if let Some(tx) = &self.mode_tx {
            let _ = tx.send(mode);
        }
    }

    /// Drive the engine until the HA event channel closes (Ctrl-C path closes
    /// it). `ctrl_rx` is the Phase 5 control channel (HA component `/control`
    /// WS → `ControlCommand`); a closed/absent control channel is benign (its
    /// arm just never fires).
    pub async fn run(
        mut self,
        mut rx: mpsc::Receiver<HaEvent>,
        mut ctrl_rx: mpsc::Receiver<ControlCommand>,
    ) {
        // The forensic (Opus) pass runs in spawned tasks that post results back
        // here, so the live loop never blocks on it. All CaseState mutation stays
        // in this single task (the results arrive as a select arm), so there is no
        // shared-state locking.
        let (id_tx, mut id_rx) = mpsc::channel::<IdResult>(64);
        self.id_tx = Some(id_tx);

        // Seed the initial alarm mode: `state_changed` only fires on CHANGES, so
        // query the alarm's current state once at startup (an already-armed alarm
        // should show the standby pane immediately; an already-triggered one opens
        // a case). Best-effort — a failure just means we wait for the first event.
        match self.rest.state(&self.cfg.alarm_entity).await {
            Ok(Some(s)) => {
                let mode = AlarmMode::from_ha(&s);
                let entity_id = self.cfg.alarm_entity.clone();
                self.handle(HaEvent::AlarmModeChanged { entity_id, mode }).await;
            }
            Ok(None) => {}
            Err(e) => warn!("could not read initial alarm state: {e:#}"),
        }

        loop {
            if self.active.is_some() {
                // Target a fixed FRAME PERIOD: wait only the part of the cadence the
                // previous tick (the LLM calls) didn't already consume. A tick that
                // ran >= cadence fires the next immediately (no pad); a fast tick is
                // padded up to the cadence. So frames aim for ~cadence_secs apart.
                let cadence = Duration::from_secs(self.current_cadence());
                let wait = cadence.saturating_sub(self.last_tick_duration);
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(ev) => self.handle(ev).await,
                        None => break,
                    },
                    Some(cmd) = ctrl_rx.recv() => self.handle_control(cmd).await,
                    Some(res) = id_rx.recv() => self.handle_identification(res),
                    _ = tokio::time::sleep(wait) => {
                        if let Err(e) = self.tick().await {
                            error!("assessment tick failed: {e:#}");
                        }
                    }
                }
            } else {
                // The authorised-dwell timer (if armed): when it elapses, revert the
                // HUD off the green AUTHORISED state. `pending()` (never resolves)
                // when no dwell.
                let dwell = self.revert_pending.map(|(t, _)| t);
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(ev) => self.handle(ev).await,
                        None => break,
                    },
                    Some(cmd) = ctrl_rx.recv() => self.handle_control(cmd).await,
                    // Drain any late forensic result that lands after standdown
                    // (dropped as stale inside the handler).
                    Some(res) = id_rx.recv() => self.handle_identification(res),
                    _ = async move {
                        match dwell {
                            Some(t) => tokio::time::sleep_until(t).await,
                            None => std::future::pending::<()>().await,
                        }
                    } => {
                        if let Some((_, kind)) = self.revert_pending.take() {
                            match kind {
                                // Post-incident: drop the cleared case (HUD → standby).
                                Revert::ClearCase => if self.active.is_none() {
                                    let _ = self.state_tx.send(None);
                                },
                                // No-incident: revert the green to disarmed standby.
                                Revert::ToDisarmed => self.broadcast_mode(AlarmMode::Disarmed),
                            }
                            info!("authorised dwell elapsed; HUD reverts");
                        }
                    }
                }
            }
        }
        info!("engine stopped");
    }

    /// Run a single assessment cycle without the alarm (`--once`): open an
    /// ephemeral case, run one tick, print the resulting `CaseState`, stand down.
    /// An optional `profile` override (the `--profile` flag) opens the manual case
    /// at a softer (gated) posture so the Phase-4b gating/escalation paths can be
    /// exercised without real softer-trigger HA entities.
    pub async fn run_once(&mut self, profile: Option<TriggerProfile>) -> Result<()> {
        match profile {
            Some(p) => self.open_case_with_profile("manual", p).await?,
            None => self.open_case("manual").await?,
        }
        self.tick().await?;
        if let Some(active) = &self.active {
            println!("{}", serde_json::to_string_pretty(&active.state)?);
        }
        self.standdown().await;
        Ok(())
    }

    async fn handle(&mut self, ev: HaEvent) {
        match ev {
            HaEvent::AlarmModeChanged { entity_id, mode } => {
                let prev = self.prev_mode;
                self.prev_mode = Some(mode);
                match mode {
                    AlarmMode::Triggered => {
                        self.revert_pending = None; // fresh incident supersedes any dwell
                        self.broadcast_mode(mode);
                        if self.active.is_some() {
                            return; // already running a case
                        }
                        if let Err(e) = self.open_case(&entity_id).await {
                            error!("failed to open case: {e:#}");
                            return;
                        }
                        if let Err(e) = self.tick().await {
                            error!("first assessment tick failed: {e:#}");
                        }
                    }
                    AlarmMode::Arming | AlarmMode::Armed => {
                        self.revert_pending = None; // arming/armed supersedes any dwell
                        self.broadcast_mode(mode);
                    }
                    AlarmMode::Disarmed => {
                        if self.active.is_some() {
                            // Post-incident disarm: standdown holds the green
                            // CaseState for the dwell (sets `revert_pending`).
                            self.broadcast_mode(mode);
                            self.standdown().await;
                        } else if matches!(
                            prev,
                            Some(AlarmMode::Arming | AlarmMode::Armed | AlarmMode::Triggered)
                        ) {
                            // No-incident disarm (e.g. armed → disarmed): show the
                            // green AUTHORISED "all clear" for the dwell, then revert.
                            self.broadcast_mode(AlarmMode::Authorised);
                            self.revert_pending = Some((
                                tokio::time::Instant::now()
                                    + Duration::from_secs(AUTHORISED_DWELL_SECS),
                                Revert::ToDisarmed,
                            ));
                            info!("disarmed (no incident); showing AUTHORISED for the dwell");
                        } else {
                            // Startup-disarmed / already idle → plain standby.
                            self.broadcast_mode(mode);
                        }
                    }
                    AlarmMode::Authorised => {} // engine-internal; never from HA
                }
            }
            // Phase 4b: a softer (non-alarm) trigger fired — open a GATED case with
            // its profile. If a case is already active (incl. a just-promoted one
            // that tripped the real alarm), do nothing: that case is the live one,
            // and a second softer fire mustn't open a competing case or downgrade it.
            HaEvent::TriggerFired { entity_id, profile } => {
                if self.active.is_some() {
                    info!("softer trigger {entity_id} ignored (case already active)");
                    return;
                }
                if let Err(e) = self.open_case_with_profile(&entity_id, profile).await {
                    error!("failed to open {profile:?} case for {entity_id}: {e:#}");
                    return;
                }
                if let Err(e) = self.tick().await {
                    error!("first assessment tick failed: {e:#}");
                }
            }
        }
    }

    /// Handle a Phase 5 control command. `Standdown` reuses the existing
    /// standdown path (identical to the HA `AlarmCleared` edge); `Acknowledge`
    /// records an additive timeline milestone. Both are no-ops (logged) when no
    /// case is active.
    async fn handle_control(&mut self, cmd: ControlCommand) {
        match cmd {
            ControlCommand::Standdown => {
                if self.active.is_some() {
                    info!("control: operator standdown");
                    self.standdown().await;
                } else {
                    info!("control: standdown ignored (no active case)");
                }
            }
            ControlCommand::Acknowledge => {
                if let Some(mut active) = self.active.take() {
                    let now = Utc::now();
                    active.state.push_event(
                        now,
                        TimelineKind::Acknowledged,
                        "Operator acknowledged.",
                    );
                    if let Err(e) = self.persist_and_broadcast(&mut active) {
                        error!("failed to flush acknowledgement: {e:#}");
                    }
                    self.active = Some(active);
                    info!("control: operator acknowledged");
                } else {
                    info!("control: acknowledge ignored (no active case)");
                }
            }
        }
    }

    /// Cadence in seconds, honouring the optional daily token cap.
    fn current_cadence(&self) -> u64 {
        let lc = &self.cfg.loop_config;
        // Daily token cap (if set + exceeded) forces the slow cadence.
        if let Some(cap) = lc.daily_token_cap {
            if self.daily.input_tokens >= cap {
                return lc.slow_cadence_secs;
            }
        }
        // Once the case has gone quiet long enough to decay off Critical (≥
        // DECAY_ELEVATED_SECS of no activity), ease off to the slow cadence — no
        // point hammering the cameras every few seconds when nothing is happening.
        // Fresh activity resets `last_activity`, snapping back to the fast cadence.
        if let Some(active) = &self.active {
            let idle = (Utc::now() - active.last_activity).num_seconds();
            if idle >= DECAY_ELEVATED_SECS {
                return lc.slow_cadence_secs;
            }
        }
        lc.cadence_secs
    }

    async fn open_case(&mut self, alarm_entity: &str) -> Result<()> {
        // Resolve the trigger profile for the firing entity (Phase 4a/4b). For the
        // alarm entity (or the legacy single-source config) this is `Alarm`; a
        // softer source resolves to its configured `Investigate`/`General`.
        let profile = self
            .cfg
            .trigger_profile_map()
            .get(alarm_entity)
            .copied()
            .unwrap_or(TriggerProfile::Alarm);
        self.open_case_with_profile(alarm_entity, profile).await
    }

    /// Open a case for `alarm_entity` with an EXPLICIT trigger `profile` — the
    /// softer-trigger firing path (Phase 4b) and the `--once --profile` override
    /// use this; the alarm path goes through [`open_case`] (which resolves the
    /// profile from config).
    async fn open_case_with_profile(
        &mut self,
        alarm_entity: &str,
        profile: TriggerProfile,
    ) -> Result<()> {
        // A new incident cancels any pending authorised-dwell revert.
        self.revert_pending = None;
        let now = Utc::now();
        let case_id = format!("case-{}", now.format("%Y%m%dT%H%M%SZ"));
        let base = default_case_base()?;
        let dir = CaseDir::create(&base, &case_id, now, alarm_entity)
            .context("creating case dir")?;
        let mut state = CaseState::new(case_id.clone(), now, profile);
        // Read the human location that tripped the alarm (set by the HA "Alarm
        // Sensors" automation) so the spoken breach line can name it.
        state.trigger_location = self.read_trigger_location().await;
        // The case-open detail reflects the profile. For a gated profile the
        // `CaseOpened` event is suppressed by the output gate (no voice/PD), so
        // this text only surfaces on the journal + HUD — but it must not say "Alarm
        // triggered" for a silent perimeter/doorstep assessment.
        let opened_detail = match profile {
            TriggerProfile::Alarm => "Alarm triggered; case opened.",
            TriggerProfile::Investigate => "Perimeter activity; silent assessment opened.",
            TriggerProfile::General => "Doorstep approach; quick assessment opened.",
        };
        state.push_event(now, TimelineKind::CaseOpened, opened_detail);
        info!(
            case_id = %case_id,
            dir = %dir.root().display(),
            trigger_location = ?state.trigger_location,
            trigger_profile = ?profile,
            "case opened"
        );

        // Seed the movement-announcement state: the trigger zone (where the alarm
        // tripped) is excluded from "Intruder in <zone>." lines — the breach line
        // already names it. If the trigger location is unknown, the first zone an
        // intruder is seen in becomes the (suppressed) initial zone instead.
        let mut announced_zones = HashSet::new();
        let initial_zone_recorded = match state.trigger_location.as_deref() {
            Some(t) if !t.trim().is_empty() => {
                announced_zones.insert(zone_key(t));
                true
            }
            _ => false,
        };

        let mut active = ActiveCase {
            state,
            dir,
            next_subject: 1,
            next_still: 1,
            still_quality: HashMap::new(),
            id_spoken: HashSet::new(),
            identifying: HashSet::new(),
            weapon_still: HashSet::new(),
            last_activity: now,
            tick_count: 0,
            announced_zones,
            initial_zone_recorded,
            // An `Alarm` case is born at full posture (never "promoted"); the gated
            // profiles start un-escalated and may be promoted by `evaluate_promotion`.
            escalated: profile == TriggerProfile::Alarm,
            // `Investigate` runs silent for a bounded window then quietly stands down.
            investigate_deadline: match profile {
                TriggerProfile::Investigate => {
                    Some(Instant::now() + Duration::from_secs(INVESTIGATE_DEADLINE_SECS))
                }
                _ => None,
            },
        };
        self.persist_and_broadcast(&mut active)?;
        self.active = Some(active);
        Ok(())
    }

    /// Read the configured trigger-location entity (e.g.
    /// `input_text.alarm_trigger_room`). `None` if unconfigured, unreadable, or a
    /// placeholder state (`unknown`/`unavailable`/empty).
    async fn read_trigger_location(&self) -> Option<String> {
        let entity = self.cfg.trigger_location_entity.as_deref()?;
        match self.rest.state(entity).await {
            Ok(Some(s)) => {
                let s = s.trim();
                if s.is_empty()
                    || s.eq_ignore_ascii_case("unknown")
                    || s.eq_ignore_ascii_case("unavailable")
                {
                    None
                } else {
                    Some(s.to_string())
                }
            }
            Ok(None) => None,
            Err(e) => {
                warn!("could not read trigger location {entity}: {e:#}");
                None
            }
        }
    }

    async fn standdown(&mut self) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        let now = Utc::now();
        active.state.status = CaseStatus::Standdown;
        active
            .state
            .push_event(now, TimelineKind::Standdown, "Alarm disarmed; standing down.");
        active.state.status = CaseStatus::Cleared;
        active
            .state
            .push_event(now, TimelineKind::Cleared, "Case cleared.");
        if let Err(e) = self.persist_and_broadcast(&mut active) {
            error!("failed to flush final case state: {e:#}");
        }
        if let Err(e) = active.dir.write_dossier(&active.state) {
            error!("failed to write dossier: {e:#}");
        }
        info!(case_id = %active.state.case_id, "case cleared");
        // Hold the green AUTHORISED state (the cleared CaseState) on the HUD for
        // the dwell, then the run loop broadcasts `None` (HUD → standby, kiosks
        // restore). `--once` has no run loop, so the dwell is never serviced there
        // (harmless).
        self.revert_pending = Some((
            tokio::time::Instant::now() + Duration::from_secs(AUTHORISED_DWELL_SECS),
            Revert::ClearCase,
        ));
    }

    /// One assessment cycle: capture → Sonnet live loop → merge → best stills →
    /// Opus forensic → journal + broadcast. Returns `true` when the case has gone
    /// quiet long enough to auto-close (the caller then stands it down).
    async fn tick(&mut self) -> Result<()> {
        let t0 = Instant::now();
        // Take the active case out so we can borrow `self` for the model clients.
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        let outcome = self.run_tick(&mut active).await;
        self.active = Some(active);
        // Record how long the assessment took so the loop can pace the next frame
        // to a fixed period rather than adding the full cadence on top.
        self.last_tick_duration = t0.elapsed();
        match outcome {
            // `close` = the case should end without a disarm: a 1-hour inactivity
            // auto-close (Alarm), OR a gated profile's quiet standdown (4b — the
            // specific reason is logged inside `decide_promotion`/`decay_threat`).
            Ok(true) => {
                info!("case ending (auto-close / quiet standdown); standing down");
                self.standdown().await;
                Ok(())
            }
            Ok(false) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn run_tick(&mut self, active: &mut ActiveCase) -> Result<bool> {
        let now = Utc::now();

        // Choose the camera set: the first tick — and every full_sweep_every-th
        // tick — sweeps ALL cameras for a baseline; other ticks assess only
        // cameras with recent motion (plus any tracking an intruder), so the
        // live loop stays responsive instead of re-sending every camera.
        let lc = &self.cfg.loop_config;
        let base_full = active.tick_count == 0
            || (lc.full_sweep_every > 0 && active.tick_count % lc.full_sweep_every as u64 == 0);
        // While actively tracking an intruder (someone on the roster, seen within
        // the active window), assess EVERY camera each tick — don't let motion-
        // gating hold back the room an intruder has just walked into (that gate +
        // the periodic-sweep interval was the main lag in room-change detection).
        // Cost is acceptable during a live intrusion; it reverts to motion-gating
        // once the case goes quiet (decays off Critical).
        let actively_tracking = !active.state.intruders.is_empty()
            && (now - active.last_activity).num_seconds() < DECAY_ELEVATED_SECS;
        let full = base_full || actively_tracking;
        let cameras = if full {
            self.cfg.cameras.clone()
        } else {
            self.select_active_cameras(active).await
        };
        // Idle tick (nothing moving, no LLM call): still run threat decay +
        // auto-close so a quiet case winds down on schedule.
        if !full && cameras.is_empty() {
            let close = self.decay_threat(active, now);
            self.persist_and_broadcast(active)?;
            active.tick_count += 1;
            return Ok(close);
        }

        // 1. Capture the chosen cameras concurrently; tolerate failures.
        let rest = &self.rest;
        let snaps = futures_util::future::join_all(
            cameras
                .iter()
                .map(|c| async move { (c.clone(), rest.snapshot(&c.entity).await) }),
        )
        .await;
        let mut stills: HashMap<String, Vec<u8>> = HashMap::new(); // label → jpeg
        for (cam, res) in snaps {
            match res {
                Ok(jpeg) => {
                    stills.insert(cam.label.clone(), jpeg);
                }
                Err(e) => warn!("snapshot failed for {}: {e:#}", cam.label),
            }
        }
        if stills.is_empty() {
            warn!("no camera stills available this tick; skipping assessment");
            let close = self.decay_threat(active, now);
            self.persist_and_broadcast(active)?;
            active.tick_count += 1;
            return Ok(close);
        }

        // 2. Telemetry bundle.
        let telemetry = self.rest.telemetry(&self.cfg.telemetry_entities).await;

        // 3. Sonnet live what/where loop — ONE focused call per active camera, run
        //    CONCURRENTLY. A single multi-image call splits the model's attention
        //    and dilutes per-frame scrutiny (a held knife gets missed); a focused
        //    call per frame keeps full attention on each. Cost is not a concern —
        //    responsiveness + detection quality are. Per-call failures are tolerated
        //    (warn + skip that camera); the surviving results are merged into one
        //    `LiveAssessment` before the existing merge/threat machinery runs.
        let sonnet = &self.sonnet;
        let seed = &self.seed;
        let calls = stills.iter().map(|(label, jpeg)| {
            let instruction = self.live_instruction_camera(&active.state, &telemetry, label);
            async move {
                let req = AssessRequest {
                    seed,
                    images: vec![ImageInput::camera(label, jpeg)],
                    instruction,
                    schema: Some(live_assessment_schema()),
                    max_tokens: LIVE_MAX_TOKENS,
                    reasoning: Reasoning::Fast,
                };
                (label.clone(), sonnet.assess(&req).await)
            }
        });
        let completions = futures_util::future::join_all(calls).await;

        // Keep each assessment tied to the camera it came from — the per-camera
        // intruder sightings drive the zone-movement announcement (see below)
        // BEFORE the merge collapses them to a single best-view location.
        let mut labelled: Vec<(String, LiveAssessment)> = Vec::new();
        let mut summed = Usage::default();
        for (label, res) in completions {
            match res {
                Ok(completion) => {
                    summed.add(&completion.usage);
                    match completion.parse::<LiveAssessment>() {
                        Ok(a) => labelled.push((label, a)),
                        Err(e) => warn!("parsing live assessment for {label} failed: {e:#}"),
                    }
                }
                Err(e) => warn!("sonnet live loop failed for {label}: {e:#}"),
            }
        }
        self.record_usage(&summed);
        if labelled.is_empty() {
            warn!("no usable camera assessments this tick; skipping merge");
            let close = self.decay_threat(active, now);
            self.persist_and_broadcast(active)?;
            active.tick_count += 1;
            return Ok(close);
        }

        // Zones an intruder was seen in THIS tick, keyed off each camera's own
        // sighting (first detection, any confidence) rather than the reconciled
        // best-view location — so a room change is announced the moment a frame
        // shows the intruder there, not once that camera becomes the dominant view.
        let mut zones_seen: Vec<String> = Vec::new();
        for (label, a) in &labelled {
            if !a.intruders.is_empty() {
                let zone = self.zone_for_label(label);
                if !zones_seen.contains(&zone) {
                    zones_seen.push(zone);
                }
            }
        }

        // Merge the per-camera assessments into one delta the existing machinery
        // consumes unchanged.
        let results: Vec<LiveAssessment> = labelled.into_iter().map(|(_, a)| a).collect();
        let live = merge_camera_assessments(results);

        // Was a person seen this tick? Drives activity (threat ratchet + decay).
        let model_threat = live.threat_level;
        let person_seen = live.person_detected
            || !live.intruders.is_empty()
            || live.locations.iter().any(|l| l.person_present);

        // 4. Merge the delta into CaseState (does NOT touch threat level).
        self.merge_live(active, live, full, now);
        // Announce intruder movement into a fresh zone (off this tick's per-camera
        // sightings — earliest reliable signal).
        self.announce_zone_entries(active, &zones_seen, now);
        if active.state.status == CaseStatus::Triggered {
            active.state.status = CaseStatus::Assessing;
        }

        // 5. Best stills + Opus forensic for new/improved intruders (async). A
        // non-escalated `General` case is a SHALLOW, Sonnet-only glance — it must
        // not spend Opus (it fires on every doorstep visitor). Once it escalates
        // (`effective_profile` flips to `Alarm`), the forensic pass runs as normal.
        if active.state.effective_profile != TriggerProfile::General || active.escalated {
            self.upgrade_intruders(active, &stills, now).await;
        }

        // 6. Threat model. Up only via the model/weapon WHILE active; down only via
        // the inactivity decay (below). Never let the model de-escalate.
        if person_seen {
            active.last_activity = now;
            let armed = active.state.intruders.iter().any(|i| i.armed);
            let candidate = if armed { ThreatLevel::Critical } else { model_threat };
            self.raise_threat(active, candidate, now);
        }
        let mut close = self.decay_threat(active, now);

        // 7. Escalation policy (Phase 4b). For a GATED (`Investigate`/`General`)
        // case, decide per-tick whether to PROMOTE it to full `Alarm` posture (which
        // trips the real alarm + ungates all outputs) or quietly STAND DOWN. A
        // promotion happens inside `evaluate_promotion`; it returns `true` only for
        // a quiet auto-standdown (nothing of concern within the profile's budget).
        // No-op for an already-`Alarm`/escalated case.
        if self.evaluate_promotion(active, now).await? {
            close = true;
        }

        active.state.updated_at = now;
        info!(
            case_id = %active.state.case_id,
            profile = ?active.state.effective_profile,
            threat = ?active.state.threat_level,
            intruders = active.state.intruders.len(),
            in_tok = summed.total_input(),
            out_tok = summed.output_tokens,
            cache_read = summed.cache_read_input_tokens,
            "tick: {}",
            active.state.summary
        );
        self.persist_and_broadcast(active)?;
        active.tick_count += 1;
        Ok(close)
    }

    /// Set the threat level (emitting a `ThreatLevelChanged` milestone) if it
    /// actually changes. The single mutation point for `threat_level`.
    fn set_threat(
        &self,
        active: &mut ActiveCase,
        new: ThreatLevel,
        now: DateTime<Utc>,
        reason: &str,
    ) {
        if active.state.threat_level != new {
            let detail = format!(
                "Threat level {:?} → {:?} ({reason}).",
                active.state.threat_level, new
            );
            active.state.threat_level = new;
            active.state.push_event(now, TimelineKind::ThreatLevelChanged, detail);
        }
    }

    /// Ratchet the threat level UP to `candidate` (never down — that is the
    /// decay's job).
    fn raise_threat(&self, active: &mut ActiveCase, candidate: ThreatLevel, now: DateTime<Utc>) {
        if candidate > active.state.threat_level {
            self.set_threat(active, candidate, now, "assessed");
        }
    }

    /// Apply the inactivity decay and report whether the case should auto-close.
    /// During an active incident the threat only ever comes DOWN here: capped at
    /// `Elevated` after `DECAY_ELEVATED_SECS` of no activity, `Info` after
    /// `DECAY_LOW_SECS`; the case auto-closes after `AUTO_CLOSE_SECS`.
    fn decay_threat(&self, active: &mut ActiveCase, now: DateTime<Utc>) -> bool {
        let idle = (now - active.last_activity).num_seconds();
        if idle >= AUTO_CLOSE_SECS {
            return true;
        }
        let ceiling = if idle >= DECAY_LOW_SECS {
            ThreatLevel::Info
        } else if idle >= DECAY_ELEVATED_SECS {
            ThreatLevel::Elevated
        } else {
            ThreatLevel::Critical // no cap inside the active window
        };
        if active.state.threat_level > ceiling {
            self.set_threat(active, ceiling, now, "inactivity");
        }
        false
    }

    /// PROMOTE a gated case to full `Alarm` posture (Phase 4b). Idempotent —
    /// guarded on `active.escalated`, so the real-alarm trip + the `Escalated`
    /// milestone fire AT MOST once. The locked user decisions: a self-escalated
    /// case goes to FULL `Alarm` (klaxon included) AND Argus trips the real
    /// `alarm_control_panel`, cascading the legacy whole-house automations.
    ///
    /// Steps: flip `effective_profile` to `Alarm`, push the `Escalated` milestone
    /// (the gate lets THIS event through → 4b's breach voice/klaxon/PD fire off it),
    /// broadcast (so the ungated state reaches the consumers), THEN trip the real
    /// alarm. After this `gated()` is false, so subsequent events route normally.
    async fn promote(&mut self, active: &mut ActiveCase, reason: &str, now: DateTime<Utc>) {
        // Idempotent state mutation (flip posture + push the `Escalated` milestone).
        // Returns false if it was already promoted — then there's nothing to do.
        if !mark_promoted(active, reason, now) {
            return;
        }
        // Ratchet up so the freshly-promoted case doesn't immediately decay.
        if active.state.threat_level < ThreatLevel::Elevated {
            self.set_threat(active, ThreatLevel::Elevated, now, "escalated");
        }
        info!(
            case_id = %active.state.case_id,
            reason,
            "ESCALATED — promoting gated case to Alarm + tripping the real alarm panel"
        );
        // Broadcast the ungated state FIRST so the outputs consumer sees the
        // `Escalated` milestone on a case whose `gated()` is now false.
        if let Err(e) = self.persist_and_broadcast(active) {
            error!("failed to flush escalation: {e:#}");
        }
        // Trip the real house alarm — the locked decision. This produces an
        // `AlarmModeChanged{Triggered}` which `handle` no-ops (a case is already
        // active), so it never opens a second case.
        let entity = self.cfg.primary_alarm_entity();
        if let Err(e) = self
            .rest
            .call_service(
                "alarm_control_panel",
                "alarm_trigger",
                serde_json::json!({ "entity_id": entity }),
            )
            .await
        {
            error!("failed to trip the real alarm panel {entity}: {e:#}");
        }
    }

    /// The Phase-4b escalation decision, run once per tick for an ACTIVE case.
    /// No-op for an already-`Alarm`/escalated case (returns `false`). For a gated
    /// (`Investigate`/`General`) case it either promotes (full alarm) or reports a
    /// quiet auto-standdown (`true`) when the profile's silent budget elapses with
    /// nothing of concern.
    ///
    /// - **Always-escalate (any profile):** an armed intruder, `threat_level ==
    ///   Critical`, or a resident-in-danger signal (the threat-ratchet condition).
    /// - **`General`:** also escalate on an OBVIOUS threat indicator (weapon /
    ///   face-concealment / balaclava); else quiet standdown after
    ///   `GENERAL_STANDDOWN_TICKS`.
    /// - **`Investigate`:** escalate on a confirmed non-resident behaving as an
    ///   intruder / attempted entry; else quiet standdown when
    ///   `investigate_deadline` passes.
    async fn evaluate_promotion(&mut self, active: &mut ActiveCase, now: DateTime<Utc>) -> Result<bool> {
        match decide_promotion(active) {
            PromotionDecision::Continue => Ok(false),
            PromotionDecision::Standdown => Ok(true),
            PromotionDecision::Promote(reason) => {
                self.promote(active, reason, now).await;
                Ok(false)
            }
        }
    }

    /// Cameras to assess on a non-baseline tick: those with recent motion, any
    /// with no motion sensors configured (can't gate → always watched), and any
    /// camera currently tracking a detected intruder.
    async fn select_active_cameras(
        &self,
        active: &ActiveCase,
    ) -> Vec<crate::config::CameraConfig> {
        let window = self.cfg.loop_config.motion_window_secs;
        let tracked: std::collections::HashSet<String> = active
            .state
            .intruders
            .iter()
            .filter_map(|i| i.best_camera.clone())
            .collect();
        let mut out = Vec::new();
        for cam in &self.cfg.cameras {
            if cam.motion_entities.is_empty() || tracked.contains(&cam.label) {
                out.push(cam.clone());
                continue;
            }
            let mut moving = false;
            for e in &cam.motion_entities {
                if self.rest.sensor_active(e, window).await {
                    moving = true;
                    break;
                }
            }
            if moving {
                out.push(cam.clone());
            }
        }
        out
    }

    /// Build the Sonnet instruction for ONE camera's focused assessment. Scopes the
    /// model to the single frame `label` (so it gives that frame full attention),
    /// while still passing telemetry + the current intruder roster (so it reuses
    /// existing `subject-N` ids). The RESIDENT RECOGNITION and WEAPONS & THREATS
    /// clauses are kept verbatim from the original multi-image instruction.
    ///
    /// The OPENING posture is parameterised by `state.effective_profile` (Phase
    /// 4b): an `Alarm` case is hunting an intruder (today's text); `Investigate`
    /// vets whether perimeter activity is a prowler vs a resident/known visitor;
    /// `General` is a quick glance for OBVIOUS danger at a friendly zone.
    fn live_instruction_camera(&self, state: &CaseState, telemetry: &str, label: &str) -> String {
        let mut roster = String::new();
        if state.intruders.is_empty() {
            roster.push_str("No intruders detected yet.");
        } else {
            roster.push_str("Current intruder roster (reuse these ids for the same people):\n");
            for i in &state.intruders {
                roster.push_str(&format!(
                    "- {}: {} (confidence {:.2}, last seen {})\n",
                    i.id,
                    i.descriptors,
                    i.confidence,
                    i.location.as_deref().unwrap_or("unknown")
                ));
            }
        }
        // Per-profile opening posture (Phase 4b). The structured output + the
        // RESIDENT RECOGNITION / WEAPONS & THREATS clauses below are identical
        // across profiles — only the framing of WHY Argus is looking changes.
        let posture = match state.effective_profile {
            TriggerProfile::Alarm => {
                "The intruder alarm is active; assume an intrusion is in progress."
            }
            TriggerProfile::Investigate => {
                "This is a PERIMETER SECURITY assessment while the premises is secured. Activity \
                 has been detected on an external camera. Your job is to vet whether this is a \
                 PROWLER (a non-resident loitering, trying doors or windows, attempting entry, \
                 masked, or otherwise behaving like an intruder) versus a benign presence (a \
                 resident arriving home, a known visitor, or a delivery). Do not assume an \
                 intrusion — assess it."
            }
            TriggerProfile::General => {
                "This is a QUICK doorstep check at a FRIENDLY zone (someone approaching). Assume a \
                 benign visitor. Glance for OBVIOUS danger ONLY — a brandished weapon, a balaclava \
                 or concealed face, or clear forced-entry behaviour. A delivery, a resident, or an \
                 ordinary visitor is benign; do not over-read it."
            }
        };
        format!(
            "{posture} You are assessing a \
             SINGLE camera named \"{label}\" — the one still above. Report ONLY what is visible in \
             this one frame: your `locations` should describe just this camera, and your \
             `intruders`/`threats` only the people and threats visible in it. Return an updated \
             structured situation report for this frame.\n\n{telemetry}\n\n{roster}\n\n\
             RESIDENT RECOGNITION (critical). The premises briefing names the residents. Only treat \
             a person as a resident — and therefore EXCLUDE them from `intruders` — when their \
             appearance matches a named resident BEYOND REASONABLE DOUBT. A mere resemblance \
             (similar build, hair, skin tone, or clothing) is NOT enough: someone who only looks \
             like a resident is still an intruder. When in any doubt, classify as an intruder — \
             fail toward intruder. For every person who is NOT a near-certain resident, either \
             reuse an existing roster id or coin the next `subject-N`.\n\n\
             WEAPONS & THREATS (critical). For EVERY person, look closely at their hands and \
             anything they are holding or carrying. Set `armed` to true if they hold a weapon OR any \
             object that could plausibly be one — a knife or blade, firearm, bat, stick, screwdriver \
             or other tool, bottle, or an elongated / pointed / metallic object whose shape or grip \
             is consistent with a weapon. Name it in `weapon`, hedging when unsure (e.g. \"kitchen \
             knife\", \"possible knife\", \"elongated object, possibly a blade\"). Do NOT dismiss a \
             hard-to-make-out handheld object as harmless — during an active intrusion a plausible \
             weapon must be flagged. (Only an object clearly recognised as benign — a phone, remote, \
             or cup — is not a weapon.) List every weapon or threat in `threats`, and set \
             `threat_level` to `critical` whenever a weapon is present or a resident appears to be in \
             danger.\n\n\
             Report only what this frame supports. If no person is visible in this camera, return \
             empty `intruders` and set `person_detected` to false."
        )
    }

    /// Normalise a camera reference (which the model returns as either the human
    /// label or the entity id — entity ids appear in the seed's camera map) to the
    /// camera LABEL, which is the key used for stills + the camera maps.
    fn to_label(&self, camref: &str) -> String {
        self.entity_to_label
            .get(camref)
            .cloned()
            .unwrap_or_else(|| camref.to_string())
    }

    /// `to_label` for an optional reference.
    fn to_label_opt(&self, camref: Option<String>) -> Option<String> {
        camref.map(|c| self.to_label(&c))
    }

    /// Reconcile a live assessment delta into the case state, emitting timeline
    /// milestones for changes Argus cares about.
    fn merge_live(
        &self,
        active: &mut ActiveCase,
        live: LiveAssessment,
        full: bool,
        now: chrono::DateTime<Utc>,
    ) {
        active.state.summary = live.summary;

        // NOTE: threat level is NOT set here — the engine ratchets it up (active)
        // and decays it down (inactivity) in `run_tick`, never from the model
        // mid-incident. `live.threat_level` is read by the caller as the ratchet
        // candidate.

        // Threats: a full sweep re-baselines the whole list (the model just saw
        // every camera); a partial tick only saw some cameras, so union its
        // observations with what we already had rather than dropping the rest.
        let threats: Vec<String> = live
            .threats
            .into_iter()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        if full {
            let mut deduped: Vec<String> = Vec::new();
            for t in threats {
                if !deduped.contains(&t) {
                    deduped.push(t);
                }
            }
            active.state.threats = deduped;
        } else {
            for t in threats {
                if !active.state.threats.contains(&t) {
                    active.state.threats.push(t);
                }
            }
        }

        // Locations: UPSERT by label so a partial (motion-gated) tick only
        // updates the cameras it actually assessed; zones not in this delta keep
        // their last-known observation rather than being wiped.
        for l in live.locations {
            // The model may return either the label ("Kitchen") or the entity id
            // (it's in the seed's camera map); normalise to the label so the key
            // matches the stills map + the camera↔label maps.
            let label = self.to_label(&l.camera_label);
            let camera = self
                .label_to_entity
                .get(&label)
                .cloned()
                .unwrap_or_else(|| label.clone());
            if let Some(existing) = active.state.locations.iter_mut().find(|o| o.label == label) {
                existing.camera = camera;
                existing.activity = l.activity;
                existing.person_present = l.person_present;
                existing.last_seen = now;
            } else {
                active.state.locations.push(LocationObservation {
                    camera,
                    label,
                    activity: l.activity,
                    person_present: l.person_present,
                    last_seen: now,
                });
            }
        }

        // Intruders: reconcile by stable id.
        for d in live.intruders {
            if let Some(existing) = active.state.intruder_mut(&d.id) {
                existing.confidence = d.confidence;
                existing.id_quality = d.id_quality;
                existing.location = self.to_label_opt(d.location);
                existing.activity = d.activity;
                // Don't let the fast loop clobber the firmer Opus descriptors.
                if !existing.identified {
                    existing.descriptors = d.descriptors;
                }
                existing.best_camera = self.to_label_opt(d.best_camera_label);
                // Weapons latch ON: a person seen armed stays armed even if a later
                // frame occludes the weapon. Emit a milestone on the OFF→ON edge.
                if d.armed {
                    let newly_armed = !existing.armed;
                    existing.armed = true;
                    if existing.weapon.is_none() {
                        existing.weapon = d.weapon.clone();
                    }
                    if newly_armed {
                        let id = existing.id.clone();
                        let weapon = existing.weapon.clone();
                        Self::push_weapon_event(active, &id, weapon.as_deref(), now);
                    }
                }
            } else {
                // Honour the model's id if it coined a fresh subject-N; bump our
                // counter past it so we never collide.
                if let Some(n) = d.id.strip_prefix("subject-").and_then(|s| s.parse::<u32>().ok()) {
                    self_next_subject_at_least(active, n + 1);
                }
                let id = if d.id.is_empty() {
                    let id = format!("subject-{}", active.next_subject);
                    active.next_subject += 1;
                    id
                } else {
                    d.id.clone()
                };
                active.state.push_event(
                    now,
                    TimelineKind::IntruderDetected,
                    format!("Intruder {id} detected: {}", d.descriptors),
                );
                active.state.intruders.push(Intruder {
                    id: id.clone(),
                    descriptors: d.descriptors,
                    confidence: d.confidence,
                    id_quality: d.id_quality,
                    location: self.to_label_opt(d.location),
                    activity: d.activity,
                    armed: d.armed,
                    weapon: d.weapon.clone(),
                    best_stills: Vec::new(),
                    identified: false,
                    dossier: None,
                    spoken_summary: None,
                    best_camera: self.to_label_opt(d.best_camera_label),
                });
                // A first sighting that is already armed earns the weapon milestone too.
                if d.armed {
                    Self::push_weapon_event(active, &id, d.weapon.as_deref(), now);
                }
            }
        }
    }

    /// Announce intruder movement into a fresh zone. `zones_seen` are the zones an
    /// intruder was sighted in THIS tick (one per camera that saw them, deduped) —
    /// the earliest signal, taken before the per-camera assessments are merged to a
    /// single best-view location. The first time a zone appears that hasn't been
    /// announced (and isn't the excluded initial/trigger zone) push an
    /// `IntruderEnteredZone` milestone — the voice gate renders it "Intruder in
    /// <zone>.". Zone-level dedup, so several intruders entering one zone speak once.
    fn announce_zone_entries(
        &self,
        active: &mut ActiveCase,
        zones_seen: &[String],
        now: chrono::DateTime<Utc>,
    ) {
        for display in zones_seen {
            let key = zone_key(display);
            if key.is_empty() || active.announced_zones.contains(&key) {
                continue;
            }
            active.announced_zones.insert(key);
            // With no known trigger location, the first observed zone IS the
            // initial location → record + suppress it; announce the rest.
            if !active.initial_zone_recorded {
                active.initial_zone_recorded = true;
                continue;
            }
            active.state.push_event(
                now,
                TimelineKind::IntruderEnteredZone,
                format!("Intruder entered {display}."),
            );
        }
    }

    /// The zone (room/area) a camera `label` watches — its configured `zone`, or
    /// the label itself when unset.
    fn zone_for_label(&self, label: &str) -> String {
        self.label_to_zone
            .get(label)
            .cloned()
            .unwrap_or_else(|| label.to_string())
    }

    /// Push a `WeaponDetected` milestone for `id`, naming the weapon if known.
    fn push_weapon_event(
        active: &mut ActiveCase,
        id: &str,
        weapon: Option<&str>,
        now: chrono::DateTime<Utc>,
    ) {
        let detail = match weapon.map(str::trim).filter(|w| !w.is_empty()) {
            Some(w) => format!("{id} is armed: {w}."),
            None => format!("{id} is armed."),
        };
        active.state.push_event(now, TimelineKind::WeaponDetected, detail);
    }

    /// For each intruder whose best view improved — or who is newly armed (so we
    /// grab the weapon frame for the kiosk + forensic pass) — save a fresh still
    /// and SCHEDULE the Opus forensic pass. The Opus call runs CONCURRENTLY (a
    /// spawned task posting back over `id_tx`) so the live loop keeps its cadence;
    /// in `--once` (no `id_tx`) it runs inline so the printed state is complete.
    async fn upgrade_intruders(
        &mut self,
        active: &mut ActiveCase,
        stills: &HashMap<String, Vec<u8>>,
        now: chrono::DateTime<Utc>,
    ) {
        // Collect the work first (immutable scan), then mutate.
        let mut jobs: Vec<(String, String, Vec<u8>)> = Vec::new(); // (id, camera_label, jpeg)
        for intruder in &active.state.intruders {
            // Don't queue a second forensic pass while one is already in flight.
            if active.identifying.contains(&intruder.id) {
                continue;
            }
            // Pick the CLEAREST frame for the forensic pass: `best_camera` now
            // tracks the highest-`id_quality` view (set in the merge). The live
            // model often leaves it null on early ticks, so fall back to the
            // intruder's current location, then any person-present camera, so Opus
            // still starts on the FIRST detection.
            let Some(label) = pick_still_label(intruder, stills, &active.state.locations) else {
                continue;
            };
            let Some(jpeg) = stills.get(&label) else {
                continue;
            };
            // (Re)profile when the FRAME got meaningfully clearer than the one we
            // last profiled — chase a better picture rather than a confidence
            // wobble. First sighting always profiles (so the record fills fast).
            let last = active.still_quality.get(&intruder.id).copied();
            let clearer = match last {
                None => true,
                Some(prev) => intruder.id_quality >= prev + QUALITY_IMPROVE,
            };
            // Always capture a still the first time someone is seen armed, so the
            // weapon frame reaches the kiosk + Opus even if the frame was no clearer.
            let weapon_capture = intruder.armed && !active.weapon_still.contains(&intruder.id);
            if clearer || weapon_capture {
                if weapon_capture {
                    active.weapon_still.insert(intruder.id.clone());
                }
                jobs.push((intruder.id.clone(), label, jpeg.clone()));
            }
        }

        for (id, label, jpeg) in jobs {
            // Save the still.
            let still_id = format!("still-{:04}", active.next_still);
            active.next_still += 1;
            let camera = self
                .label_to_entity
                .get(&label)
                .cloned()
                .unwrap_or_else(|| label.clone());
            if let Err(e) = active.dir.save_still(&still_id, &jpeg) {
                error!("failed to save still {still_id}: {e:#}");
                continue;
            }
            let still_ref = StillRef {
                id: still_id,
                camera,
                captured_at: now,
            };
            if let Some(intruder) = active.state.intruder_mut(&id) {
                intruder.best_stills.insert(0, still_ref);
                intruder.best_stills.truncate(MAX_BEST_STILLS);
                active.still_quality.insert(id.clone(), intruder.id_quality);
            }
            active.state.push_event(
                now,
                TimelineKind::BestStillUpgraded,
                format!("Clearer still captured for {id}."),
            );

            // Schedule the Opus forensic pass.
            active.identifying.insert(id.clone());
            match self.id_tx.clone() {
                // Daemon: spawn so the live loop never blocks on Opus.
                Some(tx) => {
                    let opus = self.opus.clone();
                    let seed = self.seed.clone();
                    let residents = self.resident_photos.clone();
                    let case_id = active.state.case_id.clone();
                    tokio::spawn(async move {
                        let (ident, usage) =
                            run_identify(&opus, &seed, &residents, &label, &jpeg).await;
                        let _ = tx
                            .send(IdResult {
                                case_id,
                                intruder_id: id,
                                ident,
                                usage,
                            })
                            .await;
                    });
                }
                // `--once`: run inline so the printed CaseState is complete.
                None => {
                    let (ident, usage) = run_identify(
                        &self.opus,
                        &self.seed,
                        &self.resident_photos,
                        &label,
                        &jpeg,
                    )
                    .await;
                    if let Some(u) = &usage {
                        self.record_usage(u);
                    }
                    active.identifying.remove(&id);
                    match ident {
                        Ok(ident) => self.apply_identification(active, &id, ident, now),
                        Err(e) => warn!("opus identification failed for {id}: {e:#}"),
                    }
                }
            }
        }
    }

    /// Apply a completed forensic result (from a spawned Opus task) to the engine-
    /// owned case. Drops a stale result whose case has already been cleared. All
    /// CaseState mutation lives on the engine task, so there is no locking.
    fn handle_identification(&mut self, res: IdResult) {
        if let Some(u) = &res.usage {
            self.record_usage(u);
        }
        let Some(mut active) = self.active.take() else {
            return; // no active case; late result, drop
        };
        if active.state.case_id != res.case_id {
            // Result from a since-cleared case — discard, restore the current one.
            self.active = Some(active);
            return;
        }
        active.identifying.remove(&res.intruder_id);
        let now = Utc::now();
        match res.ident {
            Ok(ident) => {
                self.apply_identification(&mut active, &res.intruder_id, ident, now);
                if let Err(e) = self.persist_and_broadcast(&mut active) {
                    error!("failed to flush identification: {e:#}");
                }
            }
            Err(e) => warn!("opus identification failed for {}: {e:#}", res.intruder_id),
        }
        self.active = Some(active);
    }

    /// Merge an `Identification` into intruder `id`: firm descriptors/dossier,
    /// latch armed/weapon (emitting `WeaponDetected` on the OFF→ON edge), push the
    /// `IntruderIdentified` milestone ONCE (on the first identification), and
    /// ratchet the threat to Critical on a fresh weapon confirmation.
    fn apply_identification(
        &self,
        active: &mut ActiveCase,
        id: &str,
        ident: Identification,
        now: chrono::DateTime<Utc>,
    ) {
        let mut newly_armed = false;
        let mut weapon_for_event: Option<String> = None;
        // The forensic pass re-runs as clearer stills arrive (firming up
        // descriptors). The `IntruderIdentified` milestone — which drives the
        // spoken profile + the PagerDuty page — fires ONCE per intruder, and only
        // once the identification is solid (`confidence >= ID_SPEAK_CONF`), so the
        // announced profile isn't a guess off a first-glimpse frame. Every pass
        // still updates the record (descriptors/dossier/best-still) silently.
        let confidence;
        if let Some(intruder) = active.state.intruder_mut(id) {
            intruder.descriptors = ident.descriptors;
            intruder.dossier = Some(ident.dossier);
            let summary = ident.spoken_summary.trim();
            if !summary.is_empty() {
                intruder.spoken_summary = Some(summary.to_string());
            }
            intruder.confidence = intruder.confidence.max(ident.confidence);
            intruder.identified = true;
            confidence = intruder.confidence;
            // The forensic pass can be what first confirms a weapon a grainy live
            // frame missed. Latch armed ON; never clear it.
            if ident.armed {
                newly_armed = !intruder.armed;
                intruder.armed = true;
                if intruder.weapon.is_none() {
                    intruder.weapon = ident.weapon.clone();
                }
                weapon_for_event = intruder.weapon.clone();
            }
        } else {
            return; // intruder gone (shouldn't happen mid-case)
        }
        // Announce (+ page) the first time we have a confident-enough profile.
        if confidence >= ID_SPEAK_CONF && active.id_spoken.insert(id.to_string()) {
            active.state.push_event(
                now,
                TimelineKind::IntruderIdentified,
                format!("{id} identified: {}", ident.distinguishing_features),
            );
        }
        if newly_armed {
            Self::push_weapon_event(active, id, weapon_for_event.as_deref(), now);
            // A forensic weapon confirmation is fresh activity → ratchet to Critical.
            active.last_activity = now;
            self.raise_threat(active, ThreatLevel::Critical, now);
        }
    }

    fn record_usage(&mut self, usage: &Usage) {
        let today = Utc::now().date_naive();
        if today != self.daily.date {
            self.daily.date = today;
            self.daily.input_tokens = 0;
        }
        self.daily.input_tokens += usage.total_input();
    }

    fn persist_and_broadcast(&self, active: &mut ActiveCase) -> Result<()> {
        active.dir.append_event(&active.state).context("journalling case event")?;
        let _ = self.state_tx.send(Some(active.state.clone()));
        Ok(())
    }
}

/// Merge the per-camera live assessments (one focused Sonnet call per active
/// camera) into ONE `LiveAssessment` the existing `merge_live`/threat machinery
/// consumes unchanged. `results` is guaranteed non-empty by the caller.
///
/// - `threat_level`: MAX across calls (`Info < Elevated < Critical`).
/// - `person_detected`: logical OR.
/// - `locations`: concatenated (each call contributes its own camera).
/// - `threats`: concatenated, trimmed, empties dropped, first occurrence kept.
/// - `intruders`: reconciled by `id` — see `merge_intruders`.
/// - `summary`: the per-camera summaries of cameras that saw a person, joined
///   with "; "; a clear-all line if none did.
fn merge_camera_assessments(results: Vec<LiveAssessment>) -> LiveAssessment {
    let threat_level = results
        .iter()
        .map(|r| r.threat_level)
        .max()
        .unwrap_or(ThreatLevel::Info);
    let person_detected = results.iter().any(|r| r.person_detected);

    let mut locations: Vec<LocationDelta> = Vec::new();
    let mut threats: Vec<String> = Vec::new();
    let mut summaries: Vec<String> = Vec::new();
    for r in &results {
        locations.extend(r.locations.iter().cloned());
        for t in &r.threats {
            let t = t.trim();
            if !t.is_empty() && !threats.iter().any(|e| e == t) {
                threats.push(t.to_string());
            }
        }
        if r.person_detected || !r.intruders.is_empty() {
            let s = r.summary.trim();
            if !s.is_empty() {
                summaries.push(s.to_string());
            }
        }
    }

    let intruders = merge_intruders(&results);

    let summary = if summaries.is_empty() {
        "No persons detected on the assessed cameras.".to_string()
    } else {
        summaries.join("; ")
    };

    LiveAssessment {
        summary,
        threat_level,
        person_detected,
        locations,
        intruders,
        threats,
    }
}

/// Reconcile the per-camera intruder deltas by stable `id` into one delta each,
/// preserving order of first appearance. Within an id group:
/// - `armed` = OR (any camera that saw them armed wins);
/// - `weapon` = first non-empty weapon, preferring one from an armed sighting;
/// - `confidence` = MAX; `id_quality` = MAX (best frame for ID anywhere this tick);
/// - `descriptors` = from the highest-confidence sighting;
/// - `location`/`activity` = from the highest-confidence sighting;
/// - `best_camera_label` = the camera of the highest-`id_quality` sighting (so the
///   forensic pass profiles on the CLEAREST view), falling back to any non-null one.
fn merge_intruders(results: &[LiveAssessment]) -> Vec<IntruderDelta> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<IntruderDelta>> = HashMap::new();
    for r in results {
        for d in &r.intruders {
            if !groups.contains_key(&d.id) {
                order.push(d.id.clone());
            }
            groups.entry(d.id.clone()).or_default().push(d.clone());
        }
    }

    let mut out: Vec<IntruderDelta> = Vec::with_capacity(order.len());
    for id in order {
        let group = &groups[&id];
        // Highest-confidence sighting (first wins ties) drives descriptors +
        // location/activity/best_camera.
        let best = group
            .iter()
            .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
            .expect("group is non-empty");
        let armed = group.iter().any(|d| d.armed);
        let confidence = group.iter().map(|d| d.confidence).fold(f32::MIN, f32::max);
        let id_quality = group.iter().map(|d| d.id_quality).fold(f32::MIN, f32::max);
        // Prefer a weapon from an armed sighting; else any non-empty weapon.
        let weapon = group
            .iter()
            .filter(|d| d.armed)
            .find_map(|d| non_empty(&d.weapon))
            .or_else(|| group.iter().find_map(|d| non_empty(&d.weapon)));
        // best_camera_label: the camera with the CLEAREST view of this person (max
        // id_quality) so the forensic pass profiles on the best shot; fall back to
        // its reported location, then any non-null best_camera in the group.
        let clearest = group
            .iter()
            .max_by(|a, b| a.id_quality.total_cmp(&b.id_quality))
            .expect("group is non-empty");
        let best_camera_label = clearest
            .best_camera_label
            .clone()
            .or_else(|| clearest.location.clone())
            .or_else(|| group.iter().find_map(|d| d.best_camera_label.clone()));

        out.push(IntruderDelta {
            id,
            descriptors: best.descriptors.clone(),
            confidence,
            id_quality,
            location: best.location.clone(),
            activity: best.activity.clone(),
            best_camera_label,
            armed,
            weapon,
        });
    }
    out
}

/// Return a trimmed non-empty clone of an optional string, else `None`.
fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Normalise a zone name to a comparison key (trimmed, lower-cased) so the
/// trigger zone seeded from HA's area name and a camera's configured zone match
/// despite case/whitespace differences.
fn zone_key(zone: &str) -> String {
    zone.trim().to_lowercase()
}

/// The Phase-4b per-tick escalation verdict for a gated case (pure of IO so it is
/// unit-testable; [`Engine::evaluate_promotion`] acts on it).
#[derive(Debug, PartialEq, Eq)]
enum PromotionDecision {
    /// Keep assessing (silent), no change this tick.
    Continue,
    /// Promote to full `Alarm` posture (trips the real alarm) — carries the reason.
    Promote(&'static str),
    /// Quiet auto-standdown (the profile's silent budget elapsed with nothing of
    /// concern). The caller stands the case down with no outputs (still gated).
    Standdown,
}

/// Decide the escalation verdict for an ACTIVE case this tick. No-op
/// (`Continue`) for an already-`Alarm`/escalated case. For a gated profile:
/// - **Always-escalate (any profile):** an armed intruder, `threat_level ==
///   Critical`, or a resident-in-danger signal.
/// - **`General`:** also escalate on an OBVIOUS threat indicator (weapon /
///   face-concealment / balaclava); else quiet standdown once `tick_count` reaches
///   `GENERAL_STANDDOWN_TICKS`.
/// - **`Investigate`:** escalate on a confirmed intruder behaving as a prowler;
///   else quiet standdown once `investigate_deadline` passes.
fn decide_promotion(active: &ActiveCase) -> PromotionDecision {
    if active.escalated || !active.state.gated() {
        return PromotionDecision::Continue;
    }
    let state = &active.state;

    // Always-escalate set (shared by both gated profiles).
    if state.intruders.iter().any(|i| i.armed) {
        return PromotionDecision::Promote("armed intruder");
    }
    if state.threat_level >= ThreatLevel::Critical {
        return PromotionDecision::Promote("critical threat");
    }
    if resident_in_danger(state) {
        return PromotionDecision::Promote("resident in danger");
    }

    match state.effective_profile {
        TriggerProfile::Alarm => PromotionDecision::Continue, // unreachable (gated ruled out)
        TriggerProfile::General => {
            if obvious_threat(state) {
                PromotionDecision::Promote("obvious threat at door")
            } else if active.tick_count + 1 >= GENERAL_STANDDOWN_TICKS {
                // +1: this is the (tick_count)-th tick about to complete.
                PromotionDecision::Standdown
            } else {
                PromotionDecision::Continue
            }
        }
        TriggerProfile::Investigate => {
            if intruder_behaviour(state) {
                PromotionDecision::Promote("prowler behaviour")
            } else if active
                .investigate_deadline
                .is_some_and(|d| Instant::now() >= d)
            {
                PromotionDecision::Standdown
            } else {
                PromotionDecision::Continue
            }
        }
    }
}

/// Idempotently mark a case PROMOTED to full `Alarm` posture (Phase 4b): set
/// `escalated`, flip `effective_profile` to `Alarm`, refresh `last_activity` (the
/// escalation is fresh justified activity), and push the `Escalated` milestone.
/// Returns `true` if it actually promoted, `false` if the case was already
/// promoted (the caller then skips the trip + broadcast). Pure of network/IO so
/// it's unit-testable; [`Engine::promote`] wraps it with the threat ratchet,
/// broadcast, and the real-alarm trip.
fn mark_promoted(active: &mut ActiveCase, reason: &str, now: DateTime<Utc>) -> bool {
    if active.escalated {
        return false;
    }
    active.escalated = true;
    active.state.effective_profile = TriggerProfile::Alarm;
    active.last_activity = now;
    active.state.push_event(
        now,
        TimelineKind::Escalated,
        format!("Case escalated to full alarm ({reason})."),
    );
    true
}

/// Does any free-text `threats` line (or the summary) contain one of `needles`
/// (case-insensitive substring)? The live model writes danger signs as free text
/// (`threats`) plus a one-line `summary`; the escalation policy scans both. Used
/// to detect resident-in-danger / face-concealment / forced-entry indicators the
/// structured fields don't capture as a boolean.
fn threats_mention(state: &CaseState, needles: &[&str]) -> bool {
    let mut hay = state.summary.to_lowercase();
    for t in &state.threats {
        hay.push('\n');
        hay.push_str(&t.to_lowercase());
    }
    needles.iter().any(|n| hay.contains(n))
}

/// A resident appears to be in apparent danger — the always-escalate signal that
/// is independent of an intruder being armed (e.g. a person grabbed, threatened,
/// or a struggle visible). Read from the free-text threat observations.
fn resident_in_danger(state: &CaseState) -> bool {
    threats_mention(
        state,
        &[
            "resident in danger",
            "in danger",
            "being attacked",
            "under threat",
            "struggle",
            "hostage",
            "assault",
        ],
    )
}

/// An OBVIOUS threat indicator at a friendly zone (the `General` escalation bar):
/// a visibly brandished weapon, face concealment (balaclava / mask), or
/// forced-entry behaviour. `armed`/`Critical` are handled by the always-escalate
/// set; this catches the obvious-but-not-yet-Critical signals from the free text.
fn obvious_threat(state: &CaseState) -> bool {
    threats_mention(
        state,
        &[
            "balaclava",
            "mask",
            "masked",
            "face conceal",
            "face cover",
            "covered face",
            "concealed face",
            "hood pulled",
            "weapon",
            "knife",
            "firearm",
            "forced entry",
            "forcing",
            "prying",
            "breaking",
        ],
    )
}

/// A confirmed non-resident behaving as an intruder (the `Investigate` escalation
/// bar): a detected intruder present AND behaving like a prowler — trying doors /
/// windows, attempted entry, loitering at an access point, or the obvious-threat
/// signals. A bare person on a perimeter camera is NOT enough; the model must read
/// intruder-like behaviour (it excludes near-certain residents from `intruders`).
fn intruder_behaviour(state: &CaseState) -> bool {
    if state.intruders.is_empty() {
        return false;
    }
    obvious_threat(state)
        || threats_mention(
            state,
            &[
                "trying door",
                "trying the door",
                "trying window",
                "attempted entry",
                "attempting entry",
                "tampering",
                "loitering",
                "prowler",
                "casing",
                "climbing",
                "intruder",
            ],
        )
}

/// Ensure the subject counter is at least `n` (used when the model coins ids).
fn self_next_subject_at_least(active: &mut ActiveCase, n: u32) {
    if active.next_subject < n {
        active.next_subject = n;
    }
}

/// Choose the camera label whose still feeds the forensic pass for `intruder`,
/// from the frames captured THIS tick (`stills`, keyed by camera label). Tries,
/// in order: the model's `best_camera`, the intruder's current `location`, any
/// person-present location we captured, then — if only one camera was captured —
/// that one. `None` means we can't tie this intruder to a frame yet.
fn pick_still_label(
    intruder: &Intruder,
    stills: &HashMap<String, Vec<u8>>,
    locations: &[LocationObservation],
) -> Option<String> {
    if let Some(bc) = &intruder.best_camera {
        if stills.contains_key(bc) {
            return Some(bc.clone());
        }
    }
    if let Some(loc) = &intruder.location {
        if stills.contains_key(loc) {
            return Some(loc.clone());
        }
    }
    for l in locations {
        if l.person_present && stills.contains_key(&l.label) {
            return Some(l.label.clone());
        }
    }
    if stills.len() == 1 {
        return stills.keys().next().cloned();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{CaseState, Intruder};

    /// Build a bare `ActiveCase` for the escalation-decision tests. The `CaseDir`
    /// is rooted in a unique per-test temp dir (the decision/state helpers under
    /// test don't touch it, but `ActiveCase` requires one).
    fn active_case(profile: TriggerProfile) -> ActiveCase {
        let now = Utc::now();
        let case_id = format!("case-test-{}", uuid_like());
        let base = std::env::temp_dir().join("argus-test").join(&case_id);
        let dir = CaseDir::create(&base, &case_id, now, "manual").expect("temp case dir");
        ActiveCase {
            state: CaseState::new(case_id, now, profile),
            dir,
            next_subject: 1,
            next_still: 1,
            still_quality: HashMap::new(),
            id_spoken: HashSet::new(),
            identifying: HashSet::new(),
            weapon_still: HashSet::new(),
            last_activity: now,
            tick_count: 0,
            announced_zones: HashSet::new(),
            initial_zone_recorded: false,
            escalated: profile == TriggerProfile::Alarm,
            investigate_deadline: match profile {
                TriggerProfile::Investigate => {
                    Some(Instant::now() + Duration::from_secs(INVESTIGATE_DEADLINE_SECS))
                }
                _ => None,
            },
        }
    }

    /// A cheap unique-ish id for temp-dir naming (avoids a `uuid`/`tempfile` dep).
    fn uuid_like() -> u128 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        t ^ ((n as u128) << 96)
    }

    fn intruder(id: &str, armed: bool) -> Intruder {
        Intruder {
            id: id.to_string(),
            descriptors: "test".to_string(),
            confidence: 0.9,
            id_quality: 0.8,
            location: None,
            activity: None,
            armed,
            weapon: if armed { Some("knife".to_string()) } else { None },
            best_stills: Vec::new(),
            identified: false,
            dossier: None,
            spoken_summary: None,
            best_camera: None,
        }
    }

    /// A `General` case with nothing of concern QUIETLY STANDS DOWN once it reaches
    /// the tick budget — and never promotes (so no outputs ever fired: it stays
    /// gated, no `Escalated` milestone).
    #[test]
    fn general_quietly_stands_down_after_budget() {
        let mut c = active_case(TriggerProfile::General);
        // Below the budget → keep assessing.
        c.tick_count = 0;
        assert_eq!(decide_promotion(&c), PromotionDecision::Continue);
        // At the budget edge → quiet standdown.
        c.tick_count = GENERAL_STANDDOWN_TICKS - 1;
        assert_eq!(decide_promotion(&c), PromotionDecision::Standdown);
        // It never escalated, so it stayed gated the whole time (no outputs).
        assert!(c.state.gated());
        assert!(!c.escalated);
        assert!(!c
            .state
            .timeline
            .iter()
            .any(|e| e.kind == TimelineKind::Escalated));
    }

    /// An ARMED intruder promotes ANY gated profile (the always-escalate set).
    #[test]
    fn armed_intruder_promotes_any_profile() {
        for profile in [TriggerProfile::General, TriggerProfile::Investigate] {
            let mut c = active_case(profile);
            c.state.intruders.push(intruder("subject-1", true));
            assert_eq!(
                decide_promotion(&c),
                PromotionDecision::Promote("armed intruder"),
                "{profile:?} with an armed intruder must promote",
            );
            // Applying the promotion flips the posture to Alarm + pushes Escalated.
            assert!(mark_promoted(&mut c, "armed intruder", Utc::now()));
            assert_eq!(c.state.effective_profile, TriggerProfile::Alarm);
            assert!(c
                .state
                .timeline
                .iter()
                .any(|e| e.kind == TimelineKind::Escalated));
        }
    }

    /// A promoted case is NO LONGER gated — its outputs flow from then on.
    #[test]
    fn promoted_case_is_not_gated() {
        let mut c = active_case(TriggerProfile::Investigate);
        assert!(c.state.gated(), "an Investigate case opens gated");
        assert!(mark_promoted(&mut c, "prowler behaviour", Utc::now()));
        assert!(!c.state.gated(), "a promoted case ungates");
        // `mark_promoted` is idempotent: a second call does nothing (no double
        // Escalated milestone, no re-trip).
        assert!(!mark_promoted(&mut c, "again", Utc::now()));
        assert_eq!(
            c.state
                .timeline
                .iter()
                .filter(|e| e.kind == TimelineKind::Escalated)
                .count(),
            1,
            "Escalated fires exactly once (idempotent)"
        );
    }

    /// An `Alarm`-profile case is never subject to the escalation policy (it's born
    /// escalated/ungated) — the reconciliation invariant that a real-alarm case is
    /// unchanged by Phase 4b.
    #[test]
    fn alarm_case_is_never_promoted_or_stood_down() {
        let mut c = active_case(TriggerProfile::Alarm);
        assert!(c.escalated);
        assert!(!c.state.gated());
        c.state.intruders.push(intruder("subject-1", true));
        c.tick_count = 100;
        assert_eq!(decide_promotion(&c), PromotionDecision::Continue);
    }

    /// `General` escalates on an OBVIOUS threat indicator (e.g. a balaclava) read
    /// from the free-text threats — before the budget runs out.
    #[test]
    fn general_promotes_on_obvious_threat() {
        let mut c = active_case(TriggerProfile::General);
        c.state.threats.push("person wearing a balaclava".to_string());
        assert_eq!(
            decide_promotion(&c),
            PromotionDecision::Promote("obvious threat at door")
        );
    }

    /// `Investigate` escalates on a detected intruder behaving as a prowler (trying
    /// a door), NOT on a bare presence.
    #[test]
    fn investigate_promotes_on_prowler_behaviour() {
        // Bare presence with no intruder roster → keep assessing.
        let mut c = active_case(TriggerProfile::Investigate);
        c.state.threats.push("someone trying the door handle".to_string());
        assert_eq!(
            decide_promotion(&c),
            PromotionDecision::Continue,
            "no detected intruder yet → don't escalate on text alone"
        );
        // With a detected (non-resident) intruder + prowler behaviour → promote.
        c.state.intruders.push(intruder("subject-1", false));
        assert_eq!(
            decide_promotion(&c),
            PromotionDecision::Promote("prowler behaviour")
        );
    }

    /// The escalation helpers' keyword scans (resident-in-danger / obvious-threat).
    #[test]
    fn threat_keyword_helpers() {
        let mut c = active_case(TriggerProfile::General);
        assert!(!resident_in_danger(&c.state));
        c.state.summary = "A resident appears to be in danger".to_string();
        assert!(resident_in_danger(&c.state));

        let mut c2 = active_case(TriggerProfile::General);
        c2.state.threats.push("visible KNIFE in hand".to_string());
        assert!(obvious_threat(&c2.state));
    }
}
