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
use chrono_tz::Tz;
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::case::{
    default_case_base, identification_schema, live_assessment_schema, CaseDir, CaseState,
    CaseStatus, Identification, LiveAssessment, LocationDelta, LocationObservation, Person,
    PersonDelta, StillRef, ThreatLevel, TimelineKind, TriggerProfile, CONCERN_INTRUDER_FLOOR,
};
use crate::config::Config;
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

/// `Investigate` standdown budget: a gated `Investigate` case that sees nothing
/// of concern auto-closes quietly after this many ticks. Kept small — Investigate
/// is a fast, shallow double-check of presumed-benign activity (it fires on every
/// front-door visitor / delivery), so it must stay cheap and silent.
const INVESTIGATE_STANDDOWN_TICKS: u64 = 4;

/// `in_duress` confidence at or above which a person counts as a structured
/// duress signal — lifts the threat to `LifeThreatening` (the structured set that
/// can reach the top tier) and escalates a gated `Investigate` case. A high bar:
/// only a confident duress read forces it; a faint/uncertain read does not.
const DURESS_FLOOR: f32 = 0.75;

/// **Escalation persistence gate (the production false-escalation safety win).**
/// A GATED (`Investigate`/`General`) case must see its escalate condition hold for
/// at least this many CONSECUTIVE ticks before it is promoted to full `Alarm`
/// (which trips the real `alarm_control_panel` + pages PagerDuty + cascades the
/// whole-house automations). A single tick's read — e.g. a phone misread as a blade
/// for one frame — must NOT trip the house; two consecutive concurring ticks must.
/// An intermittent signal (met, not-met, met) never reaches the threshold (the
/// streak resets to 0 on any non-escalating tick).
///
/// **This applies ONLY to the gated → Alarm promotion.** A real `Alarm`-profile
/// case is born escalated/ungated; `decide_promotion` short-circuits to `Continue`
/// for it, so the streak never increments and zero latency is added to the real
/// house-alarm path.
const ESCALATION_PERSISTENCE_TICKS: u32 = 2;

/// What the authorised-dwell timer does when it elapses.
#[derive(Debug, Clone, Copy)]
enum Revert {
    /// Post-incident: drop the cleared case off the broadcast (HUD → standby).
    ClearCase,
    /// No-incident disarm: revert the transient `Authorised` mode to `Disarmed`.
    ToDisarmed,
}

/// The universal **system prompt** — prepended to the private premises seed to
/// form the cached `system` block on every Sonnet/Opus call (the seed supplies
/// the `< residence summary >` / `< residents summary >` content referenced at the
/// end). Holds the role, the threat-level rubric, and the persistent assessment
/// rules so the per-scenario message prompts can stay short.
pub const SYSTEM_PROMPT: &str = "You are a smart-security AI that reviews camera footage to \
    produce a risk assessment of an incident at a residence. You are given a single camera still \
    and must determine whether anyone is attempting to enter, or has already entered, the premises, \
    and whether any person appears to be under duress.\n\n\
    For each frame, report:\n\
    - a holistic threat level for the scene;\n\
    - the people present and, for each, the likelihood they are a resident, a guest, or an intruder \
    (independent 0.0-1.0 confidences that need not sum to 1);\n\
    - any threats and any injuries you observe;\n\
    - an activity summary of what is happening in the frame, paying special attention to anything \
    potentially malicious;\n\
    - a terse list of malicious actions in the frame, e.g. \"breaking into car\", \"smashed window\".\n\n\
    Determine the threat level as follows:\n\
    - benign: no sign of intrusion, threat, or duress — a resident, an expected guest or delivery, \
    or no one of concern.\n\
    - threat_present: a genuine threat is present — a non-resident attempting or making entry, \
    forcing or testing a door or window, prowling or casing the property, or otherwise behaving as \
    an intruder, armed or not.\n\
    - life_threatening: immediate danger to life — a person is armed, appears to be under duress, or \
    appears to be seriously injured.\n\n\
    How readily to treat an unknown person as a threat depends on the situation, and is stated in \
    the instruction for this frame.\n\n\
    Assessing people (important): assign each person a resident, a guest, and an intruder \
    confidence. Only assign a high resident confidence when the person clearly matches a named \
    resident in the briefing below — a mere resemblance (similar build, hair, skin tone, or \
    clothing) is not a match. When you are uncertain, weight your confidence toward intruder: fail \
    toward caution.\n\n\
    Weapons: look closely at every person's hands and anything they hold or carry. Set `armed` if \
    they hold a weapon, or any object plausibly consistent with one (a blade, firearm, bat, stick, \
    tool, or an elongated, pointed, or metallic object), and name it in `weapon`, hedging when \
    unsure. Only an object clearly recognised as benign (a phone, remote, or cup) is not a weapon.\n\n\
    Injuries and duress: record any visible injury in a person's `injury` field (e.g. \"bleeding\", \
    \"stab wound\", \"gunshot wound\", \"unconscious\", \"possibly deceased\"). Set `in_duress` \
    (0.0-1.0) for any person who appears coerced, restrained, threatened, or in distress.";

/// Investigate scenario opener for the camera that fired the trigger. Posture:
/// presumed-benign, so the bar for a threat is HIGH (benign unless confident).
const INVESTIGATE_TRIGGER_PROMPT: &str = "Benign activity has been detected on this camera — most \
    likely a guest arriving, a delivery, or a resident returning. Treat the scene as `benign` UNLESS \
    you have HIGH confidence it is genuinely a threat — an obvious weapon, a masked or prowling \
    intruder, or forced entry — in which case report `threat_present` (or `life_threatening` if a \
    weapon, duress, or injury is present). When in doubt, it is benign.";

/// Investigate scenario opener for the other (ancillary) cameras during a case.
const INVESTIGATE_ANCILLARY_PROMPT: &str = "Benign activity has been detected elsewhere on the \
    premises, likely a guest or a delivery. You are reviewing an unrelated camera; normal activity \
    is expected — residents or guests on the property. Treat the scene as `benign` UNLESS you have \
    HIGH confidence of a genuine threat — an obvious weapon, a masked or prowling intruder, forced \
    entry, or someone in duress — in which case report `threat_present` (or `life_threatening` for a \
    weapon, duress, or injury). When in doubt, it is benign.";

/// Alarm scenario opener (any camera) — a confirmed breach is assumed, so an
/// unknown person IS a threat (the opposite posture to Investigate).
const ALARM_PROMPT: &str = "An alarm has been triggered — an intrusion is assumed. Treat any person \
    who is not clearly a resident or an expected guest as a threat: report at least `threat_present`, \
    and `life_threatening` if anyone is armed, appears to be under duress, or appears injured. \
    Determine who is present and, for each, their likelihood of being a resident, a guest, or an \
    intruder; note any weapons, injuries, or signs of duress, and any malicious activity.";

/// The forensic (Opus) instruction — shared by the inline (`--once`) and the
/// spawned (daemon) identification paths.
const IDENTIFY_INSTRUCTION: &str = "The image is the clearest still of a detected subject (the \
    camera frame). Produce a firm forensic identification of this subject: physical descriptors, \
    distinguishing features, and a short dossier paragraph for the security station. ALSO write \
    `spoken_summary` — a single short sentence (max ~18 words) suitable to be read aloud over a \
    security speaker (sex, approx age, build, key clothing, one standout feature; no preamble). \
    Determine whether the person is armed — examine the hands and any held object CLOSELY (this is \
    the clearest frame, your best chance to confirm or rule out a weapon the live pass was unsure \
    about). Set `armed` true if they hold a weapon or any object plausibly consistent with one \
    (blade, firearm, bat, stick, tool, or an elongated/pointed/metallic object) and name it in \
    `weapon`, hedging if unsure. Note any visible `injury`. Anchor every claim on the image.";

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
/// inline OR inside a spawned task with a cloned client + system prompt.
async fn run_identify(
    opus: &AnthropicClient,
    system: &str,
    label: &str,
    jpeg: &[u8],
) -> (Result<Identification>, Option<Usage>) {
    let req = AssessRequest {
        system,
        images: vec![ImageInput::camera(label, jpeg)],
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
    /// Person ids whose injury has already been announced (the `InjuryDetected`
    /// milestone fires once per person).
    injury_announced: HashSet<String>,
    /// Distinct malicious-activity strings already announced this case (the
    /// `MaliciousActivity` milestone fires once per distinct action).
    announced_malicious: HashSet<String>,
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
    /// Consecutive-tick count for which a gated case's escalate condition has held
    /// (Phase 4b precision pass). Promotion only fires once this reaches
    /// [`ESCALATION_PERSISTENCE_TICKS`] — so a single misread frame can't trip the
    /// real house alarm. Reset to 0 on any tick the escalate condition is NOT met.
    /// Irrelevant for an `Alarm` case (born escalated → `decide_promotion` is a
    /// no-op there, so this stays 0).
    escalation_streak: u32,
    /// Rank of the MAIN-image still currently held for the HUD primary pane
    /// (life_threatening=3 / malicious=2 / intruder=1 / any=0). Reset per case;
    /// a newer still only replaces the main image when its rank ≥ this
    /// (sticky-downward — see [`should_replace_main`] / [`compute_main_rank`]).
    main_still_rank: u8,
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
    /// The cached `system` block for the LIVE (Sonnet) loop: the universal
    /// [`SYSTEM_PROMPT`] + the private premises seed, assembled once at startup.
    system: String,
    /// The cached `system` block for the OPUS forensic call: the premises seed
    /// ALONE. The live `SYSTEM_PROMPT` frames the per-frame assessment task, which
    /// confuses the forensic ID call (it stubbed `dossier`/`spoken_summary` to
    /// "placeholder"); the forensic instruction defines its own task, so it gets
    /// the neutral seed only.
    seed: String,
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
        system: String,
        seed: String,
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
            system,
            seed,
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

                // Take the case into a LOCAL so a preempted (dropped) tick future
                // can't lose it: the tick borrows `&mut active`, so if the select
                // resolves on an HA/control/id arm instead, the dropped tick future
                // just ends the borrow — `active` is still owned here and is restored
                // below. (Contrast `tick()`, which `self.active.take()`s and only
                // restores at the end — dropping THAT future mid-flight would strand
                // the case as `None`. That path is kept only for `run_once`.)
                let mut active = self.active.take().expect("active.is_some() checked");

                enum Step {
                    Ticked(Result<bool>, Duration),
                    Ha(Option<HaEvent>),
                    Ctrl(ControlCommand),
                    Id(IdResult),
                }

                let step = {
                    let tick_fut = async {
                        tokio::time::sleep(wait).await;
                        let t0 = Instant::now();
                        let r = self.run_tick(&mut active).await;
                        (r, t0.elapsed())
                    };
                    tokio::pin!(tick_fut);
                    tokio::select! {
                        (r, dur) = &mut tick_fut => Step::Ticked(r, dur),
                        maybe = rx.recv() => Step::Ha(maybe),
                        Some(cmd) = ctrl_rx.recv() => Step::Ctrl(cmd),
                        Some(res) = id_rx.recv() => Step::Id(res),
                    }
                }; // tick_fut dropped here → the &mut self / &mut active borrows end

                // Restore the case BEFORE handling the event so the disarm fast-path
                // (handle → standdown) finds it and flushes Cleared/dossier/AUTHORISED.
                self.active = Some(active);

                match step {
                    Step::Ticked(r, dur) => {
                        self.last_tick_duration = dur;
                        match r {
                            Ok(true) => {
                                info!("case ending (auto-close / quiet standdown); standing down");
                                self.standdown().await;
                            }
                            Ok(false) => {}
                            Err(e) => error!("assessment tick failed: {e:#}"),
                        }
                    }
                    Step::Ha(Some(ev)) => self.handle(ev).await,
                    Step::Ha(None) => break,
                    Step::Ctrl(cmd) => self.handle_control(cmd).await,
                    Step::Id(res) => self.handle_identification(res),
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
                        // Re-arming stands a GATED (Investigate) case down: the
                        // resident has armed the house, so a silent doorstep
                        // double-check is resolved. A full `Alarm` case is left
                        // alone (re-arming mid-alarm shouldn't silently cancel it).
                        if self.active.as_ref().is_some_and(|a| a.state.gated()) {
                            info!("re-armed; standing down the active gated case");
                            self.standdown().await;
                        }
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
            // The panel-independent STANDDOWN kill switch: stand down the active
            // case (klaxon off, PD resolve, kiosk restore — same path as the
            // Phase-5 control standdown / a panel disarm). Logged either way; a
            // press with no active case is a harmless no-op.
            HaEvent::StandDownRequested { entity_id } => {
                if self.active.is_some() {
                    info!("kill switch {entity_id}: standing down active case");
                    self.standdown().await;
                } else {
                    info!("kill switch {entity_id}: no active case (no-op)");
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
        // Read the human location that tripped the case (set by the HA automation)
        // so the spoken breach line — and the trigger-vs-ancillary camera split —
        // can name it. Same entity for both profiles.
        state.trigger_location = self
            .read_location(self.cfg.trigger_location_entity.as_deref())
            .await;
        // The case-open detail reflects the profile. For a gated `Investigate` the
        // `CaseOpened` event is suppressed by the output gate (no voice/PD), so this
        // text only surfaces on the journal + HUD — it must not say "Alarm triggered"
        // for a silent doorstep double-check.
        let opened_detail = match profile {
            TriggerProfile::Alarm => "Alarm triggered; case opened.",
            TriggerProfile::Investigate => "Presumed-benign approach; silent assessment opened.",
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
            injury_announced: HashSet::new(),
            announced_malicious: HashSet::new(),
            last_activity: now,
            tick_count: 0,
            announced_zones,
            initial_zone_recorded,
            // An `Alarm` case is born at full posture (never "promoted"); a gated
            // `Investigate` starts un-escalated and may be promoted by `evaluate_promotion`.
            escalated: profile == TriggerProfile::Alarm,
            escalation_streak: 0,
            main_still_rank: 0,
        };
        self.persist_and_broadcast(&mut active)?;
        self.active = Some(active);
        Ok(())
    }

    /// Read the configured `trigger_location_entity`. `None` if unconfigured,
    /// unreadable, or a placeholder state (`unknown`/`unavailable`/empty).
    async fn read_location(&self, entity: Option<&str>) -> Option<String> {
        let entity = entity?;
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
        let actively_tracking = active
            .state
            .persons
            .iter()
            .any(|p| p.is_subject_of_concern())
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
        let system = &self.system;
        let calls = stills.iter().map(|(label, jpeg)| {
            let is_trigger = self.is_trigger_camera(label, active.state.trigger_location.as_deref());
            let instruction = self.live_instruction_camera(&active.state, &telemetry, label, is_trigger);
            async move {
                let req = AssessRequest {
                    system,
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

        // Zones a person of concern was seen in THIS tick, keyed off each camera's
        // own sighting rather than the reconciled best-view location — so a room
        // change is announced the moment a frame shows them there, not once that
        // camera becomes the dominant view.
        let mut zones_seen: Vec<String> = Vec::new();
        for (label, a) in &labelled {
            if a.persons.iter().any(delta_is_concern) {
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
            || !live.persons.is_empty()
            || live.locations.iter().any(|l| l.person_present);

        // 4. Merge the delta into CaseState (does NOT touch threat level).
        self.merge_live(active, live, full, now);

        // Announce a person-of-concern's movement into a fresh zone (off this tick's
        // per-camera sightings — earliest reliable signal).
        self.announce_zone_entries(active, &zones_seen, now);
        if active.state.status == CaseStatus::Triggered {
            active.state.status = CaseStatus::Assessing;
        }

        // 5. Best stills + Opus forensic for new/improved persons of concern
        // (async). A non-escalated `Investigate` case is a SHALLOW, Sonnet-only
        // double-check — it must not spend Opus (it fires on every front-door
        // visitor). Once it escalates (`effective_profile` flips to `Alarm`), the
        // forensic pass runs as normal.
        if active.state.effective_profile != TriggerProfile::Investigate || active.escalated {
            self.upgrade_persons(active, &stills, now).await;
        }

        // 6. Threat model. Up only via the model/structured danger WHILE active;
        // down only via the inactivity decay (below). Never let the model
        // de-escalate. `life_threatening` is reserved for STRUCTURED danger — an
        // armed person, a serious injury, or a person clearly in duress (≥
        // `DURESS_FLOOR`): such danger forces it (the floor), and WITHOUT it the
        // model's read is CAPPED at `threat_present` (the ceiling). So the top tier
        // always means a concrete life-safety signal, never the model over-calling.
        if person_seen {
            active.last_activity = now;
            let structured_danger = active.state.persons.iter().any(|p| {
                p.armed
                    || p.injury.as_deref().is_some_and(|i| !i.trim().is_empty())
                    || p.in_duress >= DURESS_FLOOR
            });
            let candidate = if structured_danger {
                ThreatLevel::LifeThreatening // floor: structured danger forces the top tier
            } else {
                model_threat.min(ThreatLevel::ThreatPresent) // ceiling: no life_threatening without it
            };
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

        // 8. MAIN image for the HUD primary pane. NOT concern-gated and NOT
        //    output-gated — the HUD/journal are never gated, so the kiosk is never
        //    blank when there is ANY activity (even an ambiguous/resident-only
        //    frame). Reuse the frames ALREADY captured this tick (no extra HA
        //    snapshot, no LLM call). Priority-ranked + sticky-downward: a newer
        //    still only replaces the held one when its rank ≥ the current rank.
        if let Some((tick_rank, category)) = compute_main_rank(&active.state) {
            if should_replace_main(active.main_still_rank, tick_rank) {
                // Pick the camera label whose frame to show. For a person of concern
                // (rank ≥ 1), prefer a concern person's best_camera then location;
                // for rank 0 (or if no concern frame resolves) any person-present
                // location; final fallback: the sole captured frame. Mirrors the
                // spirit of `pick_still_label`.
                let mut label: Option<String> = None;
                if tick_rank >= 1 {
                    for p in &active.state.persons {
                        if !p.is_subject_of_concern() {
                            continue;
                        }
                        if let Some(bc) = &p.best_camera {
                            if stills.contains_key(bc) {
                                label = Some(bc.clone());
                                break;
                            }
                        }
                        if let Some(loc) = &p.location {
                            if stills.contains_key(loc) {
                                label = Some(loc.clone());
                                break;
                            }
                        }
                    }
                }
                if label.is_none() {
                    for l in &active.state.locations {
                        if l.person_present && stills.contains_key(&l.label) {
                            label = Some(l.label.clone());
                            break;
                        }
                    }
                }
                if label.is_none() && stills.len() == 1 {
                    label = stills.keys().next().cloned();
                }
                // Only proceed if we resolved a label that has a frame this tick.
                if let Some(label) = label {
                    if let Some(jpeg) = stills.get(&label) {
                        let still_id = format!("main-{:04}", active.next_still);
                        active.next_still += 1;
                        match active.dir.save_still(&still_id, jpeg) {
                            Ok(_) => {
                                let camera = self
                                    .label_to_entity
                                    .get(&label)
                                    .cloned()
                                    .unwrap_or_else(|| label.clone());
                                active.state.main_still = Some(StillRef {
                                    id: still_id,
                                    camera,
                                    captured_at: now,
                                });
                                active.state.main_still_category = Some(category.to_string());
                                active.main_still_rank = tick_rank;
                            }
                            Err(e) => error!("failed to save main still {still_id}: {e:#}"),
                        }
                    }
                }
            }
        }

        active.state.updated_at = now;
        info!(
            case_id = %active.state.case_id,
            profile = ?active.state.effective_profile,
            threat = ?active.state.threat_level,
            persons = active.state.persons.len(),
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
            ThreatLevel::Benign
        } else if idle >= DECAY_ELEVATED_SECS {
            ThreatLevel::ThreatPresent
        } else {
            ThreatLevel::LifeThreatening // no cap inside the active window
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
        if active.state.threat_level < ThreatLevel::ThreatPresent {
            self.set_threat(active, ThreatLevel::ThreatPresent, now, "escalated");
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
        // Trip the real house alarm — the locked decision. This RAISES the real
        // panel; the resulting `alarm_control_panel` → `Triggered` is then RECONCILED
        // by `handle(AlarmModeChanged{Triggered})`, which no-ops because a case is
        // already active (`mark_promoted` flipped this case ungated above) — so the
        // panel trip is NOT re-opened as a second case. There is NO loop.
        //
        // Prefer a configurable HA SCRIPT (`escalation_alarm_script`) when set: a
        // `script.<name>` arms/triggers the panel WITH the secret code HA-side, so
        // the trip code never lives in Argus's config/seed. When unset, fall back to
        // the direct `alarm_control_panel.alarm_trigger` on the primary entity.
        let trip = match self.cfg.escalation_alarm_script.as_deref() {
            Some(script) => {
                self.rest
                    .call_service("script", script, serde_json::json!({}))
                    .await
                    .map_err(|e| format!("script.{script}: {e:#}"))
            }
            None => {
                let entity = self.cfg.primary_alarm_entity();
                self.rest
                    .call_service(
                        "alarm_control_panel",
                        "alarm_trigger",
                        serde_json::json!({ "entity_id": entity }),
                    )
                    .await
                    .map_err(|e| format!("alarm_trigger {entity}: {e:#}"))
            }
        };
        if let Err(e) = trip {
            error!("failed to trip the real alarm panel ({e})");
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
        // The persistence gate is a PURE state transition (mutates the streak,
        // returns the action) so it is unit-testable without touching the network;
        // only the actual `promote()` (which trips the real alarm) is IO.
        match apply_persistence(active, decide_promotion(active)) {
            PromotionAction::Hold => Ok(false),
            PromotionAction::Standdown => Ok(true),
            PromotionAction::Promote(reason) => {
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
            .persons
            .iter()
            .filter_map(|p| p.best_camera.clone())
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

    /// Whether camera `label` is the one that TRIGGERED an Investigate case — its
    /// zone matches the case's `trigger_location` (case-insensitive). With no known
    /// trigger location, every camera is treated as a trigger camera (more
    /// scrutiny). Selects the trigger vs ancillary Investigate prompt; irrelevant to
    /// `Alarm` (which always uses the alarm prompt).
    fn is_trigger_camera(&self, label: &str, trigger_location: Option<&str>) -> bool {
        match trigger_location.map(zone_key).filter(|t| !t.is_empty()) {
            Some(trigger) => zone_key(&self.zone_for_label(label)) == trigger,
            None => true,
        }
    }

    /// Build the per-scenario Sonnet message for ONE camera's focused assessment.
    /// The universal role + threat-rubric + assessment rules live in the cached
    /// system prompt ([`SYSTEM_PROMPT`]); this message carries the scenario opener
    /// (Alarm / Investigate trigger / Investigate ancillary), the single-camera
    /// scoping, the CONTEXT priors (local time + arm state), telemetry, and the
    /// current person roster (so ids are reused). `is_trigger` selects the
    /// Investigate trigger-vs-ancillary opener (ignored for `Alarm`).
    fn live_instruction_camera(
        &self,
        state: &CaseState,
        telemetry: &str,
        label: &str,
        is_trigger: bool,
    ) -> String {
        // CONTEXT (priors, not triggers). The current local date/time + the alarm
        // arm state inform suspicion but never decide it: a resident arriving home
        // at an odd hour is STILL a resident; an approach while armed-away (with the
        // residents out) merely weights suspicion higher. The local time also lets
        // the model reason recency against any telemetry timestamps — e.g. a
        // `event.front_door_access` entry the operator adds to `telemetry_entities`.
        let local_time = format_local_time(Utc::now(), &self.cfg.timezone);
        let arm_state = self.prev_mode.map(|m| m.describe()).unwrap_or("unknown");
        let context = format!(
            "CONTEXT (priors that inform suspicion, NOT triggers — weigh them, don't \
             decide on them). Current local time: {local_time}. House alarm: {arm_state}. \
             A resident arriving home, even at an odd hour, is still a resident; but a door \
             or perimeter approach while the house is armed-away (residents out) should weight \
             your suspicion higher. Compare any access/event timestamps in the telemetry \
             against this current local time to judge recency."
        );

        let mut roster = String::new();
        if state.persons.is_empty() {
            roster.push_str("No people detected yet.");
        } else {
            roster.push_str("Current person roster (reuse these ids for the same people):\n");
            for p in &state.persons {
                roster.push_str(&format!(
                    "- {}: {} (intruder {:.2} / guest {:.2} / resident {:.2}, last seen {})\n",
                    p.id,
                    p.descriptors,
                    p.intruder_confidence,
                    p.guest_confidence,
                    p.resident_confidence,
                    p.location.as_deref().unwrap_or("unknown")
                ));
            }
        }

        // The scenario opener: `Alarm` always uses the alarm prompt; an Investigate
        // case uses the trigger-camera prompt for the camera that fired and the
        // ancillary prompt for the rest.
        let opener = match state.effective_profile {
            TriggerProfile::Alarm => ALARM_PROMPT,
            TriggerProfile::Investigate => {
                if is_trigger {
                    INVESTIGATE_TRIGGER_PROMPT
                } else {
                    INVESTIGATE_ANCILLARY_PROMPT
                }
            }
        };

        format!(
            "{opener}\n\nYou are assessing a SINGLE camera named \"{label}\" — the still above. \
             Report ONLY what is visible in this one frame: `locations` should describe just this \
             camera, and `persons` / `threats` / `malicious_activity` only the people and things \
             visible in it.\n\n{context}\n\n{telemetry}\n\n{roster}\n\n\
             Reuse an existing roster id for a person already seen; coin the next `subject-N` for a \
             new person. If no person is visible, return empty `persons` and set `person_detected` \
             to false."
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

        // Threats + malicious activity: a full sweep re-baselines the whole list
        // (the model just saw every camera); a partial tick unions its observations
        // with what we already had rather than dropping the rest.
        merge_string_list(&mut active.state.threats, live.threats, full);
        merge_string_list(&mut active.state.malicious_activity, live.malicious_activity, full);
        // Each newly observed malicious action earns a once-off milestone.
        for action in active.state.malicious_activity.clone() {
            if active.announced_malicious.insert(action.to_lowercase()) {
                active.state.push_event(now, TimelineKind::MaliciousActivity, action);
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

        // Persons: reconcile by stable id. Each carries resident/guest/intruder
        // confidences; a person whose intruder confidence dominates is a "subject
        // of concern" — those drive the IntruderDetected milestone, voice, and the
        // forensic pass. Injuries/duress matter for ANY person (an injured resident
        // is critical too).
        for d in live.persons {
            if let Some(existing) = active.state.person_mut(&d.id) {
                existing.resident_confidence = d.resident_confidence;
                existing.guest_confidence = d.guest_confidence;
                existing.intruder_confidence = d.intruder_confidence;
                existing.id_score = d.id_score;
                existing.location = self.to_label_opt(d.location);
                existing.activity = d.activity;
                existing.in_duress = existing.in_duress.max(d.in_duress);
                // Don't let the fast loop clobber the firmer Opus descriptors.
                if !existing.identified {
                    existing.descriptors = d.descriptors;
                }
                existing.best_camera = self.to_label_opt(d.best_camera_label);
                // Weapons latch ON: a person seen armed stays armed even if a later
                // frame occludes the weapon. Stage the milestone, fire it after the
                // `existing` borrow ends (the milestone helpers re-borrow `active`).
                let mut weapon_event: Option<(String, Option<String>)> = None;
                if d.armed {
                    let newly_armed = !existing.armed;
                    existing.armed = true;
                    if existing.weapon.is_none() {
                        existing.weapon = d.weapon.clone();
                    }
                    if newly_armed {
                        weapon_event = Some((existing.id.clone(), existing.weapon.clone()));
                    }
                }
                // Injury latches ON; announce once per person (staged like the weapon).
                let mut injury_event: Option<(String, String)> = None;
                if let Some(injury) = non_empty(&d.injury) {
                    if existing.injury.is_none() {
                        existing.injury = Some(injury.clone());
                    }
                    injury_event = Some((existing.id.clone(), injury));
                }
                // `existing` is no longer used past here — the borrow ends, so the
                // milestone helpers can take `active` mutably.
                if let Some((id, weapon)) = weapon_event {
                    Self::push_weapon_event(active, &id, weapon.as_deref(), now);
                }
                if let Some((id, injury)) = injury_event {
                    Self::announce_injury(active, &id, &injury, now);
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
                // Only a person of CONCERN earns the "intruder detected" milestone;
                // a clearly resident/guest newcomer is recorded silently.
                if delta_is_concern(&d) {
                    active.state.push_event(
                        now,
                        TimelineKind::IntruderDetected,
                        format!("Intruder {id} detected: {}", d.descriptors),
                    );
                }
                let injury = non_empty(&d.injury);
                active.state.persons.push(Person {
                    id: id.clone(),
                    descriptors: d.descriptors,
                    resident_confidence: d.resident_confidence,
                    guest_confidence: d.guest_confidence,
                    intruder_confidence: d.intruder_confidence,
                    id_score: d.id_score,
                    location: self.to_label_opt(d.location),
                    activity: d.activity,
                    armed: d.armed,
                    weapon: d.weapon.clone(),
                    injury: injury.clone(),
                    in_duress: d.in_duress,
                    best_stills: Vec::new(),
                    identified: false,
                    dossier: None,
                    spoken_summary: None,
                    best_camera: self.to_label_opt(d.best_camera_label),
                });
                // A first sighting already armed / injured earns its milestone too.
                if d.armed {
                    Self::push_weapon_event(active, &id, d.weapon.as_deref(), now);
                }
                if let Some(injury) = injury {
                    Self::announce_injury(active, &id, &injury, now);
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

    /// Push an `InjuryDetected` milestone for `id` (once per person), naming the
    /// injury. The voice gate renders a casualty line and PagerDuty re-pages.
    fn announce_injury(active: &mut ActiveCase, id: &str, injury: &str, now: chrono::DateTime<Utc>) {
        if active.injury_announced.insert(id.to_string()) {
            active.state.push_event(
                now,
                TimelineKind::InjuryDetected,
                format!("{id} appears injured: {injury}."),
            );
        }
    }

    /// For each person of CONCERN whose best view improved — or who is newly armed
    /// (so we grab the weapon frame for the kiosk + forensic pass) — save a fresh
    /// still and SCHEDULE the Opus forensic pass. A clearly resident/guest person is
    /// not profiled. The Opus call runs CONCURRENTLY (a spawned task posting back
    /// over `id_tx`) so the live loop keeps its cadence; in `--once` (no `id_tx`) it
    /// runs inline so the printed state is complete.
    async fn upgrade_persons(
        &mut self,
        active: &mut ActiveCase,
        stills: &HashMap<String, Vec<u8>>,
        now: chrono::DateTime<Utc>,
    ) {
        // Collect the work first (immutable scan), then mutate.
        let mut jobs: Vec<(String, String, Vec<u8>)> = Vec::new(); // (id, camera_label, jpeg)
        for person in &active.state.persons {
            // Only profile a subject of concern (intruder-dominant); skip a clearly
            // resident/guest person — no point spending Opus to ID a known person.
            if !person.is_subject_of_concern() {
                continue;
            }
            // Don't queue a second forensic pass while one is already in flight.
            if active.identifying.contains(&person.id) {
                continue;
            }
            // Pick the CLEAREST frame for the forensic pass: `best_camera` tracks the
            // highest-`id_score` view (set in the merge). The live model often leaves
            // it null on early ticks, so fall back to the person's current location,
            // then any person-present camera, so Opus still starts on the FIRST sighting.
            let Some(label) = pick_still_label(person, stills, &active.state.locations) else {
                continue;
            };
            let Some(jpeg) = stills.get(&label) else {
                continue;
            };
            // (Re)profile when the FRAME got meaningfully clearer than the one we
            // last profiled — chase a better picture rather than a confidence
            // wobble. First sighting always profiles (so the record fills fast).
            let last = active.still_quality.get(&person.id).copied();
            let clearer = match last {
                None => true,
                Some(prev) => person.id_score >= prev + QUALITY_IMPROVE,
            };
            // Always capture a still the first time someone is seen armed, so the
            // weapon frame reaches the kiosk + Opus even if the frame was no clearer.
            let weapon_capture = person.armed && !active.weapon_still.contains(&person.id);
            if clearer || weapon_capture {
                if weapon_capture {
                    active.weapon_still.insert(person.id.clone());
                }
                jobs.push((person.id.clone(), label, jpeg.clone()));
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
            if let Some(person) = active.state.person_mut(&id) {
                person.best_stills.insert(0, still_ref);
                person.best_stills.truncate(MAX_BEST_STILLS);
                active.still_quality.insert(id.clone(), person.id_score);
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
                    // Forensic ID gets the SEED only (not the live SYSTEM_PROMPT).
                    let seed = self.seed.clone();
                    let case_id = active.state.case_id.clone();
                    tokio::spawn(async move {
                        let (ident, usage) =
                            run_identify(&opus, &seed, &label, &jpeg).await;
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

    /// Merge an `Identification` into person `id`: firm descriptors/dossier, latch
    /// armed/weapon (emitting `WeaponDetected` on the OFF→ON edge) and any injury,
    /// push the `IntruderIdentified` milestone ONCE (on the first solid id), and
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
        let mut new_injury: Option<String> = None;
        // The forensic pass re-runs as clearer stills arrive (firming up
        // descriptors). The `IntruderIdentified` milestone — which drives the
        // spoken profile + the PagerDuty page — fires ONCE per person, and only once
        // the identification is solid (`ident.confidence >= ID_SPEAK_CONF`), so the
        // announced profile isn't a guess off a first-glimpse frame. Every pass still
        // updates the record (descriptors/dossier/best-still) silently.
        if let Some(person) = active.state.person_mut(id) {
            // Guard against a degenerate forensic output (a model occasionally
            // stubs a free-text field to "placeholder" / empty): never overwrite a
            // good value with junk, and never speak/page a stub. The Sonnet
            // descriptors stand if Opus returns nothing meaningful.
            if meaningful(&ident.descriptors) {
                person.descriptors = ident.descriptors;
            }
            if meaningful(&ident.dossier) {
                person.dossier = Some(ident.dossier);
            }
            if meaningful(&ident.spoken_summary) {
                person.spoken_summary = Some(ident.spoken_summary.trim().to_string());
            }
            person.identified = true;
            // The forensic pass can be what first confirms a weapon a grainy live
            // frame missed. Latch armed ON; never clear it.
            if ident.armed {
                newly_armed = !person.armed;
                person.armed = true;
                if person.weapon.is_none() {
                    person.weapon = ident.weapon.clone();
                }
                weapon_for_event = person.weapon.clone();
            }
            // Latch any injury the forensic pass can confirm.
            if let Some(injury) = non_empty(&ident.injury) {
                if person.injury.is_none() {
                    person.injury = Some(injury.clone());
                }
                new_injury = Some(injury);
            }
        } else {
            return; // person gone (shouldn't happen mid-case)
        }
        // Announce (+ page) the first time we have a confident-enough profile.
        if ident.confidence >= ID_SPEAK_CONF && active.id_spoken.insert(id.to_string()) {
            active.state.push_event(
                now,
                TimelineKind::IntruderIdentified,
                format!("{id} identified: {}", ident.distinguishing_features),
            );
        }
        if let Some(injury) = new_injury {
            Self::announce_injury(active, id, &injury, now);
        }
        if newly_armed {
            Self::push_weapon_event(active, id, weapon_for_event.as_deref(), now);
            // A forensic weapon confirmation is fresh activity → ratchet to Critical.
            active.last_activity = now;
            self.raise_threat(active, ThreatLevel::LifeThreatening, now);
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
/// - `threats` / `malicious_activity`: concatenated, trimmed, empties dropped, deduped.
/// - `persons`: reconciled by `id` — see `merge_persons`.
/// - `summary`: the per-camera summaries of cameras that saw a person, joined
///   with "; "; a clear-all line if none did.
fn merge_camera_assessments(results: Vec<LiveAssessment>) -> LiveAssessment {
    let threat_level = results
        .iter()
        .map(|r| r.threat_level)
        .max()
        .unwrap_or(ThreatLevel::Benign);
    let person_detected = results.iter().any(|r| r.person_detected);

    let mut locations: Vec<LocationDelta> = Vec::new();
    let mut threats: Vec<String> = Vec::new();
    let mut malicious_activity: Vec<String> = Vec::new();
    let mut summaries: Vec<String> = Vec::new();
    for r in &results {
        locations.extend(r.locations.iter().cloned());
        dedup_extend(&mut threats, &r.threats);
        dedup_extend(&mut malicious_activity, &r.malicious_activity);
        if r.person_detected || !r.persons.is_empty() {
            let s = r.summary.trim();
            if !s.is_empty() {
                summaries.push(s.to_string());
            }
        }
    }

    let persons = merge_persons(&results);

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
        persons,
        threats,
        malicious_activity,
    }
}

/// Append the trimmed, non-empty, not-already-present items of `src` to `acc`.
fn dedup_extend(acc: &mut Vec<String>, src: &[String]) {
    for s in src {
        let s = s.trim();
        if !s.is_empty() && !acc.iter().any(|e| e == s) {
            acc.push(s.to_string());
        }
    }
}

/// Reconcile the per-camera person deltas by stable `id` into one delta each,
/// preserving order of first appearance. The CLEAREST sighting (max `id_score`)
/// drives the per-person reads. Within an id group:
/// - class confidences / `descriptors` / `location` / `activity`: from the clearest sighting;
/// - `armed` = OR; `weapon` = first non-empty, preferring an armed sighting;
/// - `injury` = first non-empty; `in_duress` = MAX; `id_score` = MAX;
/// - `best_camera_label` = the clearest sighting's camera, then its location, then any.
fn merge_persons(results: &[LiveAssessment]) -> Vec<PersonDelta> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, Vec<PersonDelta>> = HashMap::new();
    for r in results {
        for d in &r.persons {
            if !groups.contains_key(&d.id) {
                order.push(d.id.clone());
            }
            groups.entry(d.id.clone()).or_default().push(d.clone());
        }
    }

    let mut out: Vec<PersonDelta> = Vec::with_capacity(order.len());
    for id in order {
        let group = &groups[&id];
        // The clearest sighting (max id_score, first wins ties) drives descriptors,
        // the class confidences, and location/activity/best_camera.
        let clearest = group
            .iter()
            .max_by(|a, b| a.id_score.total_cmp(&b.id_score))
            .expect("group is non-empty");
        let armed = group.iter().any(|d| d.armed);
        let id_score = group.iter().map(|d| d.id_score).fold(f32::MIN, f32::max);
        let in_duress = group.iter().map(|d| d.in_duress).fold(f32::MIN, f32::max);
        // Prefer a weapon from an armed sighting; else any non-empty weapon.
        let weapon = group
            .iter()
            .filter(|d| d.armed)
            .find_map(|d| non_empty(&d.weapon))
            .or_else(|| group.iter().find_map(|d| non_empty(&d.weapon)));
        let injury = group.iter().find_map(|d| non_empty(&d.injury));
        let best_camera_label = clearest
            .best_camera_label
            .clone()
            .or_else(|| clearest.location.clone())
            .or_else(|| group.iter().find_map(|d| d.best_camera_label.clone()));

        out.push(PersonDelta {
            id,
            descriptors: clearest.descriptors.clone(),
            resident_confidence: clearest.resident_confidence,
            guest_confidence: clearest.guest_confidence,
            intruder_confidence: clearest.intruder_confidence,
            id_score,
            location: clearest.location.clone(),
            activity: clearest.activity.clone(),
            best_camera_label,
            armed,
            weapon,
            injury,
            in_duress,
        });
    }
    out
}

/// Merge a delta's free-text string list into the case's accumulator. A full
/// sweep re-baselines (the model just saw every camera); a partial tick unions
/// (it only saw some). Trims, drops empties, dedups (first occurrence kept).
fn merge_string_list(acc: &mut Vec<String>, delta: Vec<String>, full: bool) {
    let items: Vec<String> = delta
        .into_iter()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
        .collect();
    if full {
        let mut deduped: Vec<String> = Vec::new();
        for t in items {
            if !deduped.contains(&t) {
                deduped.push(t);
            }
        }
        *acc = deduped;
    } else {
        for t in items {
            if !acc.contains(&t) {
                acc.push(t);
            }
        }
    }
}

/// Mirror of [`Person::is_subject_of_concern`] for a live delta — true when the
/// intruder confidence dominates (and clears [`CONCERN_INTRUDER_FLOOR`]).
fn delta_is_concern(d: &PersonDelta) -> bool {
    d.intruder_confidence >= CONCERN_INTRUDER_FLOOR
        && d.intruder_confidence >= d.resident_confidence
        && d.intruder_confidence >= d.guest_confidence
}

/// Return a trimmed non-empty clone of an optional string, else `None`.
fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

/// Whether a forensic free-text field carries real content — non-empty and not a
/// stub ("placeholder"). Guards against a degenerate model output being recorded
/// or spoken (see `apply_identification`).
fn meaningful(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && !t.eq_ignore_ascii_case("placeholder")
}

/// Normalise a zone name to a comparison key (trimmed, lower-cased) so the
/// trigger zone seeded from HA's area name and a camera's configured zone match
/// despite case/whitespace differences.
fn zone_key(zone: &str) -> String {
    zone.trim().to_lowercase()
}

/// Format a UTC instant as a human local date/time in the configured `tz_name`
/// (an IANA name like `Australia/Sydney`), e.g.
/// `"Friday 20 June 2026, 11:32 (Australia/Sydney)"`. The container runs UTC, so
/// this converts explicitly via `chrono_tz` rather than `chrono::Local`. An
/// invalid/unknown `tz_name` falls back to UTC (with a `warn!`), never panics.
///
/// Used to inject the current local time into the live-assessment prompt as a
/// PRIOR (so the model can reason about odd hours, and weigh recency against
/// telemetry timestamps such as `event.front_door_access`), NOT as a trigger.
fn format_local_time(now: DateTime<Utc>, tz_name: &str) -> String {
    let tz: Tz = match tz_name.parse() {
        Ok(tz) => tz,
        Err(_) => {
            warn!("invalid timezone {tz_name:?}; falling back to UTC for prompt context");
            Tz::UTC
        }
    };
    let local = now.with_timezone(&tz);
    // %A=weekday %-d=day(no pad) %B=month %Y=year %H:%M=24h time; tz name appended.
    format!("{} ({})", local.format("%A %-d %B %Y, %H:%M"), tz_name)
}

/// The per-tick escalation verdict for a gated case (pure of IO so it is
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
/// (`Continue`) for an already-`Alarm`/escalated case. A gated `Investigate` case
/// escalates to full `Alarm` once it concludes a genuine threat — i.e. the threat
/// reaches `≥ threat_present` (an intruder, armed or not). The specific structured
/// signals (armed / injury / duress ≥ [`DURESS_FLOOR`]) are checked first for a
/// precise reason string; they also force `life_threatening` anyway. We deliberately
/// do NOT scan the model's free text (that substring-matched "no weapon visible" →
/// "weapon" and false-fired on a benign delivery driver, incident 2026-06-20).
/// With nothing of concern the shallow double-check quietly stands down once
/// `tick_count` reaches [`INVESTIGATE_STANDDOWN_TICKS`].
fn decide_promotion(active: &ActiveCase) -> PromotionDecision {
    if active.escalated || !active.state.gated() {
        return PromotionDecision::Continue;
    }
    let state = &active.state;

    // Specific structured signals first (precise reason), then the general
    // threat-present bar — a confident intruder, armed or not.
    if state.persons.iter().any(|p| p.armed) {
        return PromotionDecision::Promote("armed person");
    }
    if state
        .persons
        .iter()
        .any(|p| p.injury.as_deref().is_some_and(|i| !i.trim().is_empty()))
    {
        return PromotionDecision::Promote("person injured");
    }
    if state.persons.iter().any(|p| p.in_duress >= DURESS_FLOOR) {
        return PromotionDecision::Promote("person in duress");
    }
    if state.threat_level >= ThreatLevel::ThreatPresent {
        return PromotionDecision::Promote("threat present");
    }

    // Nothing of concern: the shallow Investigate double-check quietly stands down
    // once its tick budget elapses. (`Alarm` is unreachable here — gated ruled out.)
    if active.tick_count + 1 >= INVESTIGATE_STANDDOWN_TICKS {
        // +1: this is the (tick_count)-th tick about to complete.
        PromotionDecision::Standdown
    } else {
        PromotionDecision::Continue
    }
}

/// The action `evaluate_promotion` takes AFTER the persistence gate is applied —
/// the per-tick `PromotionDecision` filtered through the consecutive-tick streak.
#[derive(Debug, PartialEq, Eq)]
enum PromotionAction {
    /// Do nothing this tick — either the escalate condition wasn't met, or it was
    /// but the persistence streak hasn't yet reached the threshold.
    Hold,
    /// Promote to full `Alarm` (carries the reason) — the streak has held for
    /// `ESCALATION_PERSISTENCE_TICKS` consecutive ticks.
    Promote(&'static str),
    /// Quiet auto-standdown (the profile's silent budget elapsed).
    Standdown,
}

/// Apply the consecutive-tick PERSISTENCE GATE to a per-tick `PromotionDecision`,
/// mutating `active.escalation_streak`, and return the action to take. PURE of IO
/// (the network trip lives in [`Engine::promote`]), so it is unit-testable.
///
/// - `Continue`/`Standdown` ⇒ reset the streak to 0 (so an intermittent signal —
///   met, not-met, met — never accumulates) and map straight through.
/// - `Promote` ⇒ increment the streak; only emit `Promote` once it reaches
///   [`ESCALATION_PERSISTENCE_TICKS`], else `Hold` (waiting for persistence).
///
/// This is reached ONLY for a gated case: `decide_promotion` returns `Continue`
/// for an already-`Alarm`/escalated case, so the real Alarm path never increments
/// the streak and is given ZERO added latency.
fn apply_persistence(active: &mut ActiveCase, decision: PromotionDecision) -> PromotionAction {
    match decision {
        PromotionDecision::Continue => {
            active.escalation_streak = 0;
            PromotionAction::Hold
        }
        PromotionDecision::Standdown => {
            active.escalation_streak = 0;
            PromotionAction::Standdown
        }
        PromotionDecision::Promote(reason) => {
            active.escalation_streak += 1;
            if active.escalation_streak < ESCALATION_PERSISTENCE_TICKS {
                info!(
                    streak = active.escalation_streak,
                    need = ESCALATION_PERSISTENCE_TICKS,
                    reason,
                    "escalate condition met; holding for persistence before promoting"
                );
                PromotionAction::Hold
            } else {
                PromotionAction::Promote(reason)
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

/// Ensure the subject counter is at least `n` (used when the model coins ids).
fn self_next_subject_at_least(active: &mut ActiveCase, n: u32) {
    if active.next_subject < n {
        active.next_subject = n;
    }
}

/// Whether a tick's MAIN-image candidate (rank `tick_rank`) should REPLACE the
/// currently-held main image (rank `current_rank`). Sticky-downward: a newer
/// still of the same OR higher category replaces, a lower one NEVER does. Pure +
/// unit-tested.
fn should_replace_main(current_rank: u8, tick_rank: u8) -> bool {
    tick_rank >= current_rank
}

/// Compute the MAIN-image priority rank for the current case state, with the
/// category string for the HUD label. Returns `None` when there is NO activity at
/// all (no person anywhere) — the caller then leaves the existing main image as-is.
/// Ranks (high→low): 3 life_threatening / 2 malicious / 1 intruder / 0 any-person.
/// Pure + unit-tested.
fn compute_main_rank(state: &CaseState) -> Option<(u8, &'static str)> {
    if state.threat_level == ThreatLevel::LifeThreatening {
        return Some((3, "life_threatening"));
    }
    if !state.malicious_activity.is_empty() {
        return Some((2, "malicious"));
    }
    if state.persons.iter().any(|p| p.is_subject_of_concern()) {
        return Some((1, "intruder"));
    }
    let any_person =
        !state.persons.is_empty() || state.locations.iter().any(|l| l.person_present);
    if any_person {
        return Some((0, "any"));
    }
    None
}

/// Choose the camera label whose still feeds the forensic pass for `person`,
/// from the frames captured THIS tick (`stills`, keyed by camera label). Tries,
/// in order: the model's `best_camera`, the person's current `location`, any
/// person-present location we captured, then — if only one camera was captured —
/// that one. `None` means we can't tie this person to a frame yet.
fn pick_still_label(
    person: &Person,
    stills: &HashMap<String, Vec<u8>>,
    locations: &[LocationObservation],
) -> Option<String> {
    if let Some(bc) = &person.best_camera {
        if stills.contains_key(bc) {
            return Some(bc.clone());
        }
    }
    if let Some(loc) = &person.location {
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
    use crate::case::{CaseState, Person};

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
            injury_announced: HashSet::new(),
            announced_malicious: HashSet::new(),
            last_activity: now,
            tick_count: 0,
            announced_zones: HashSet::new(),
            initial_zone_recorded: false,
            escalated: profile == TriggerProfile::Alarm,
            escalation_streak: 0,
            main_still_rank: 0,
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

    /// A person of concern (intruder confidence dominant), optionally armed.
    fn person(id: &str, armed: bool) -> Person {
        Person {
            id: id.to_string(),
            descriptors: "test".to_string(),
            resident_confidence: 0.1,
            guest_confidence: 0.1,
            intruder_confidence: 0.9,
            id_score: 0.8,
            location: None,
            activity: None,
            armed,
            weapon: if armed { Some("knife".to_string()) } else { None },
            injury: None,
            in_duress: 0.0,
            best_stills: Vec::new(),
            identified: false,
            dossier: None,
            spoken_summary: None,
            best_camera: None,
        }
    }

    /// An `Investigate` case with nothing of concern QUIETLY STANDS DOWN once it
    /// reaches the tick budget — and never promotes (so no outputs ever fired: it
    /// stays gated, no `Escalated` milestone).
    #[test]
    fn investigate_quietly_stands_down_after_budget() {
        let mut c = active_case(TriggerProfile::Investigate);
        // Below the budget → keep assessing.
        c.tick_count = 0;
        assert_eq!(decide_promotion(&c), PromotionDecision::Continue);
        // At the budget edge → quiet standdown.
        c.tick_count = INVESTIGATE_STANDDOWN_TICKS - 1;
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

    /// An ARMED person promotes a gated `Investigate` case (the structured set).
    #[test]
    fn armed_person_promotes_investigate() {
        let mut c = active_case(TriggerProfile::Investigate);
        c.state.persons.push(person("subject-1", true));
        assert_eq!(
            decide_promotion(&c),
            PromotionDecision::Promote("armed person"),
        );
        // Applying the promotion flips the posture to Alarm + pushes Escalated.
        assert!(mark_promoted(&mut c, "armed person", Utc::now()));
        assert_eq!(c.state.effective_profile, TriggerProfile::Alarm);
        assert!(c
            .state
            .timeline
            .iter()
            .any(|e| e.kind == TimelineKind::Escalated));
    }

    /// A promoted case is NO LONGER gated — its outputs flow from then on.
    #[test]
    fn promoted_case_is_not_gated() {
        let mut c = active_case(TriggerProfile::Investigate);
        assert!(c.state.gated(), "an Investigate case opens gated");
        assert!(mark_promoted(&mut c, "critical threat", Utc::now()));
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
    /// escalated/ungated) — the invariant that a real-alarm case is unchanged.
    #[test]
    fn alarm_case_is_never_promoted_or_stood_down() {
        let mut c = active_case(TriggerProfile::Alarm);
        assert!(c.escalated);
        assert!(!c.state.gated());
        c.state.persons.push(person("subject-1", true));
        c.tick_count = 100;
        assert_eq!(decide_promotion(&c), PromotionDecision::Continue);
    }

    /// `Investigate` escalates once the threat reaches `≥ threat_present` (a genuine
    /// intruder, armed or not), with specific reasons for the structured signals
    /// (armed / injury / duress). A free-text concealment string ALONE does NOT
    /// promote (the free-text path false-fired on a benign delivery driver, 2026-06-20).
    #[test]
    fn investigate_promotes_on_threat_present_or_structured_danger() {
        // Free-text threat alone, threat still benign → no promotion.
        let mut c = active_case(TriggerProfile::Investigate);
        c.state.threats.push("person wearing a balaclava".to_string());
        assert_eq!(decide_promotion(&c), PromotionDecision::Continue);

        // Armed person → promote (specific reason).
        let mut armed = active_case(TriggerProfile::Investigate);
        armed.state.persons.push(person("subject-1", true));
        assert_eq!(decide_promotion(&armed), PromotionDecision::Promote("armed person"));

        // A model `threat_present` read (a confident unarmed intruder) → promote.
        let mut threat = active_case(TriggerProfile::Investigate);
        threat.state.threat_level = ThreatLevel::ThreatPresent;
        assert_eq!(decide_promotion(&threat), PromotionDecision::Promote("threat present"));
        // …and `life_threatening` likewise (≥ threat_present).
        let mut life = active_case(TriggerProfile::Investigate);
        life.state.threat_level = ThreatLevel::LifeThreatening;
        assert_eq!(decide_promotion(&life), PromotionDecision::Promote("threat present"));

        // An injured person → promote (even if not intruder-dominant: an injured
        // resident is life-threatening too).
        let mut hurt = active_case(TriggerProfile::Investigate);
        let mut p = person("subject-1", false);
        p.injury = Some("bleeding".to_string());
        hurt.state.persons.push(p);
        assert_eq!(decide_promotion(&hurt), PromotionDecision::Promote("person injured"));

        // A person clearly in duress → promote.
        let mut duress = active_case(TriggerProfile::Investigate);
        let mut p = person("subject-1", false);
        p.in_duress = 0.9;
        duress.state.persons.push(p);
        assert_eq!(decide_promotion(&duress), PromotionDecision::Promote("person in duress"));

        // A faint duress read (below the floor) does NOT promote.
        let mut faint = active_case(TriggerProfile::Investigate);
        let mut p = person("subject-1", false);
        p.in_duress = DURESS_FLOOR - 0.1;
        faint.state.persons.push(p);
        assert_eq!(decide_promotion(&faint), PromotionDecision::Continue);
    }

    /// `is_subject_of_concern`: intruder-dominant (≥ floor) is a concern; a clearly
    /// resident/guest person is not.
    #[test]
    fn subject_of_concern_classification() {
        assert!(person("subject-1", false).is_subject_of_concern());
        let mut resident = person("subject-2", false);
        resident.resident_confidence = 0.9;
        resident.intruder_confidence = 0.05;
        assert!(!resident.is_subject_of_concern());
        // Below the floor even when intruder is the max → not a concern.
        let mut faint = person("subject-3", false);
        faint.resident_confidence = 0.2;
        faint.guest_confidence = 0.2;
        faint.intruder_confidence = 0.2;
        assert!(!faint.is_subject_of_concern());
    }

    /// PERSISTENCE GATE (a): a gated case whose escalate condition holds on a
    /// SINGLE tick must NOT promote — it must hold; on the SECOND consecutive tick
    /// it DOES promote. (Drives the `apply_persistence` pure transition with a
    /// `Promote` decision, so it exercises the streak independently of the IO trip.)
    #[test]
    fn persistence_requires_two_consecutive_ticks() {
        // `ESCALATION_PERSISTENCE_TICKS` is the contract this test pins (==2).
        assert_eq!(ESCALATION_PERSISTENCE_TICKS, 2);
        let mut c = active_case(TriggerProfile::Investigate);
        // Armed person → `decide_promotion` yields Promote each tick.
        c.state.persons.push(person("subject-1", true));
        assert_eq!(decide_promotion(&c), PromotionDecision::Promote("armed person"));

        // Tick 1: condition met once → HOLD (streak 1), no promotion yet.
        let d = decide_promotion(&c);
        assert_eq!(apply_persistence(&mut c, d), PromotionAction::Hold);
        assert_eq!(c.escalation_streak, 1);

        // Tick 2: condition still met → PROMOTE (streak 2 ≥ threshold).
        let d = decide_promotion(&c);
        assert_eq!(
            apply_persistence(&mut c, d),
            PromotionAction::Promote("armed person")
        );
        assert_eq!(c.escalation_streak, 2);
    }

    /// PERSISTENCE GATE (b): an INTERMITTENT signal (met, not-met, met) never
    /// reaches the streak threshold — a single-frame misread can't accumulate into
    /// a real-alarm trip across a gap.
    #[test]
    fn intermittent_signal_never_promotes() {
        let mut c = active_case(TriggerProfile::Investigate);
        let armed = person("subject-1", true);

        // Tick 1: met → streak 1, hold.
        c.state.persons = vec![armed.clone()];
        let d = decide_promotion(&c);
        assert_eq!(apply_persistence(&mut c, d), PromotionAction::Hold);
        assert_eq!(c.escalation_streak, 1);

        // Tick 2: NOT met (person gone from this frame) → streak resets to 0.
        c.state.persons.clear();
        let d = decide_promotion(&c);
        assert_eq!(apply_persistence(&mut c, d), PromotionAction::Hold);
        assert_eq!(c.escalation_streak, 0);

        // Tick 3: met again → streak only back to 1, still NOT promoting.
        c.state.persons = vec![armed];
        let d = decide_promotion(&c);
        assert_eq!(apply_persistence(&mut c, d), PromotionAction::Hold);
        assert_eq!(c.escalation_streak, 1);
    }

    /// A `Standdown` decision also resets the streak (and maps straight through).
    #[test]
    fn standdown_resets_streak() {
        let mut c = active_case(TriggerProfile::Investigate);
        c.escalation_streak = 1;
        // No threat, at the budget edge → Standdown.
        c.tick_count = INVESTIGATE_STANDDOWN_TICKS - 1;
        let d = decide_promotion(&c);
        assert_eq!(d, PromotionDecision::Standdown);
        assert_eq!(apply_persistence(&mut c, d), PromotionAction::Standdown);
        assert_eq!(c.escalation_streak, 0);
    }

    /// PERSISTENCE GATE (c): the timezone formatting helper converts a known UTC
    /// instant to Australia/Sydney correctly. 2026-06-20T01:32:00Z is, in Sydney
    /// (AEST, UTC+10, no DST in June), Saturday 20 June 2026 at 11:32 local.
    #[test]
    fn local_time_converts_to_sydney() {
        let instant = DateTime::parse_from_rfc3339("2026-06-20T01:32:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let s = format_local_time(instant, "Australia/Sydney");
        assert_eq!(s, "Saturday 20 June 2026, 11:32 (Australia/Sydney)");

        // The same instant in UTC is two hours earlier in the day, same date.
        let utc = format_local_time(instant, "UTC");
        assert_eq!(utc, "Saturday 20 June 2026, 01:32 (UTC)");

        // An invalid tz name falls back to UTC (no panic), tagged with the bad name.
        let bad = format_local_time(instant, "Not/AZone");
        assert_eq!(bad, "Saturday 20 June 2026, 01:32 (Not/AZone)");
    }

    /// MAIN-image replacement rule (sticky-downward): a newer still of the SAME or
    /// HIGHER rank replaces; a LOWER rank never displaces a higher one.
    #[test]
    fn main_image_replacement_is_sticky_downward() {
        // Same rank replaces (newer-same-category wins).
        assert!(should_replace_main(2, 2));
        // Higher replaces lower.
        assert!(should_replace_main(1, 3));
        // Lower NEVER replaces higher.
        assert!(!should_replace_main(3, 1));
        // Baseline any-vs-any replaces.
        assert!(should_replace_main(0, 0));
    }

    /// MAIN-image rank table: each tier maps to its (rank, category), and an empty
    /// case (no person anywhere) yields `None` (leave the existing image as-is).
    #[test]
    fn compute_main_rank_covers_each_tier() {
        // Empty case → no activity → None.
        let empty = CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Alarm);
        assert_eq!(compute_main_rank(&empty), None);

        // Rank 0 (any): a benign person present (low intruder confidence), threat
        // not life-threatening, no malicious activity.
        let mut any = CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Investigate);
        let mut benign = person("subject-1", false);
        benign.resident_confidence = 0.9;
        benign.intruder_confidence = 0.05;
        benign.guest_confidence = 0.1;
        assert!(!benign.is_subject_of_concern());
        any.persons.push(benign);
        assert_eq!(compute_main_rank(&any), Some((0, "any")));

        // Rank 0 also via a person-present location with NO persons on the roster.
        let mut loc_only =
            CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Investigate);
        loc_only.locations.push(LocationObservation {
            camera: "camera.x".to_string(),
            label: "Kitchen".to_string(),
            activity: "someone moving".to_string(),
            person_present: true,
            last_seen: Utc::now(),
        });
        assert_eq!(compute_main_rank(&loc_only), Some((0, "any")));

        // Rank 1 (intruder): a person of concern present.
        let mut intruder =
            CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Investigate);
        intruder.persons.push(person("subject-1", false));
        assert_eq!(compute_main_rank(&intruder), Some((1, "intruder")));

        // Rank 2 (malicious): malicious_activity non-empty (outranks intruder).
        let mut malicious =
            CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Investigate);
        malicious.persons.push(person("subject-1", false));
        malicious.malicious_activity.push("breaking into car".to_string());
        assert_eq!(compute_main_rank(&malicious), Some((2, "malicious")));

        // Rank 3 (life_threatening): threat_level dominates everything.
        let mut life = CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Alarm);
        life.threat_level = ThreatLevel::LifeThreatening;
        life.malicious_activity.push("smashed window".to_string());
        life.persons.push(person("subject-1", true));
        assert_eq!(compute_main_rank(&life), Some((3, "life_threatening")));
    }
}
