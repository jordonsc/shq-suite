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

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{NaiveDate, Utc};
use tokio::sync::{mpsc, watch};
use tracing::{error, info, warn};

use crate::case::{
    default_case_base, identification_schema, live_assessment_schema, CaseDir, CaseState,
    CaseStatus, Identification, Intruder, LiveAssessment, LocationObservation, StillRef,
    TimelineKind,
};
use crate::config::Config;
use crate::ha::{HaEvent, RestClient};
use crate::llm::{AnthropicClient, AssessRequest, ImageInput, Reasoning, Usage};

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
/// Confidence improvement required before saving a fresh best still / re-running Opus.
const CONF_IMPROVE: f32 = 0.1;
/// Sonnet live-loop output budget (compact JSON).
const LIVE_MAX_TOKENS: u32 = 1500;
/// Opus forensic output budget (descriptors + dossier + adaptive thinking).
const ID_MAX_TOKENS: u32 = 4096;

/// The live, mutating state of one alarm episode.
struct ActiveCase {
    state: CaseState,
    dir: CaseDir,
    /// Next `subject-N` index.
    next_subject: u32,
    /// Next still file index.
    next_still: u64,
    /// Confidence at which each intruder's newest still was saved (for the
    /// "only upgrade on a meaningful improvement" heuristic).
    still_conf: HashMap<String, f32>,
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
    /// Camera label → entity id, for mapping the model's `*_label` fields back.
    label_to_entity: HashMap<String, String>,
    state_tx: watch::Sender<Option<CaseState>>,
    active: Option<ActiveCase>,
    daily: DailyUsage,
}

impl Engine {
    pub fn new(
        cfg: Config,
        rest: RestClient,
        sonnet: AnthropicClient,
        opus: AnthropicClient,
        seed: String,
        state_tx: watch::Sender<Option<CaseState>>,
    ) -> Self {
        let label_to_entity = cfg
            .cameras
            .iter()
            .map(|c| (c.label.clone(), c.entity.clone()))
            .collect();
        Self {
            cfg,
            rest,
            sonnet,
            opus,
            seed,
            label_to_entity,
            state_tx,
            active: None,
            daily: DailyUsage {
                date: Utc::now().date_naive(),
                input_tokens: 0,
            },
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
        loop {
            if self.active.is_some() {
                let cadence = self.current_cadence();
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(ev) => self.handle(ev).await,
                        None => break,
                    },
                    Some(cmd) = ctrl_rx.recv() => self.handle_control(cmd).await,
                    _ = tokio::time::sleep(Duration::from_secs(cadence)) => {
                        if let Err(e) = self.tick().await {
                            error!("assessment tick failed: {e:#}");
                        }
                    }
                }
            } else {
                tokio::select! {
                    maybe = rx.recv() => match maybe {
                        Some(ev) => self.handle(ev).await,
                        None => break,
                    },
                    Some(cmd) = ctrl_rx.recv() => self.handle_control(cmd).await,
                }
            }
        }
        info!("engine stopped");
    }

    /// Run a single assessment cycle without the alarm (`--once`): open an
    /// ephemeral case, run one tick, print the resulting `CaseState`, stand down.
    pub async fn run_once(&mut self) -> Result<()> {
        self.open_case("manual").await?;
        self.tick().await?;
        if let Some(active) = &self.active {
            println!("{}", serde_json::to_string_pretty(&active.state)?);
        }
        self.standdown().await;
        Ok(())
    }

    async fn handle(&mut self, ev: HaEvent) {
        match ev {
            HaEvent::AlarmTriggered { entity_id } => {
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
            HaEvent::AlarmCleared { .. } => {
                if self.active.is_some() {
                    self.standdown().await;
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
        match lc.daily_token_cap {
            Some(cap) if self.daily.input_tokens >= cap => lc.slow_cadence_secs,
            _ => lc.cadence_secs,
        }
    }

    async fn open_case(&mut self, alarm_entity: &str) -> Result<()> {
        let now = Utc::now();
        let case_id = format!("case-{}", now.format("%Y%m%dT%H%M%SZ"));
        let base = default_case_base()?;
        let dir = CaseDir::create(&base, &case_id, now, alarm_entity)
            .context("creating case dir")?;
        let mut state = CaseState::new(case_id.clone(), now);
        state.push_event(now, TimelineKind::CaseOpened, "Alarm triggered; case opened.");
        info!(case_id = %case_id, dir = %dir.root().display(), "case opened");

        let mut active = ActiveCase {
            state,
            dir,
            next_subject: 1,
            next_still: 1,
            still_conf: HashMap::new(),
        };
        self.persist_and_broadcast(&mut active)?;
        self.active = Some(active);
        Ok(())
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
    }

    /// One assessment cycle: capture → Sonnet live loop → merge → best stills →
    /// Opus forensic → journal + broadcast.
    async fn tick(&mut self) -> Result<()> {
        // Take the active case out so we can borrow `self` for the model clients.
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        let result = self.run_tick(&mut active).await;
        self.active = Some(active);
        result
    }

    async fn run_tick(&mut self, active: &mut ActiveCase) -> Result<()> {
        let now = Utc::now();

        // 1. Capture every camera concurrently; tolerate failures.
        let rest = &self.rest;
        let snaps = futures_util::future::join_all(
            self.cfg
                .cameras
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
            return Ok(());
        }

        // 2. Telemetry bundle.
        let telemetry = self.rest.telemetry(&self.cfg.telemetry_entities).await;

        // 3. Sonnet live what/where loop (structured output).
        let images: Vec<ImageInput> = stills
            .iter()
            .map(|(label, jpeg)| ImageInput { label, jpeg })
            .collect();
        let instruction = self.live_instruction(&active.state, &telemetry);
        let req = AssessRequest {
            seed: &self.seed,
            images,
            instruction,
            schema: Some(live_assessment_schema()),
            max_tokens: LIVE_MAX_TOKENS,
            reasoning: Reasoning::Fast,
        };
        let completion = self.sonnet.assess(&req).await.context("sonnet live loop")?;
        self.record_usage(&completion.usage);
        let live: LiveAssessment = completion.parse().context("parsing live assessment")?;

        // 4. Merge the delta into CaseState.
        self.merge_live(active, live, now);
        if active.state.status == CaseStatus::Triggered {
            active.state.status = CaseStatus::Assessing;
        }

        // 5. Best stills + Opus forensic for new/improved intruders.
        self.upgrade_intruders(active, &stills, now).await;

        active.state.updated_at = now;
        info!(
            case_id = %active.state.case_id,
            threat = ?active.state.threat_level,
            intruders = active.state.intruders.len(),
            in_tok = completion.usage.total_input(),
            out_tok = completion.usage.output_tokens,
            cache_read = completion.usage.cache_read_input_tokens,
            "tick: {}",
            active.state.summary
        );
        self.persist_and_broadcast(active)?;
        Ok(())
    }

    /// Build the Sonnet instruction: telemetry + current roster + the ask.
    fn live_instruction(&self, state: &CaseState, telemetry: &str) -> String {
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
        format!(
            "The intruder alarm is active. Assess the current camera stills above and return an \
             updated structured situation report.\n\n{telemetry}\n\n{roster}\n\n\
             For each person you can see, either reuse an existing roster id or coin the next \
             `subject-N`. Report only what the images support. If no person is visible, return \
             empty `intruders` and set `person_detected` to false."
        )
    }

    /// Reconcile a live assessment delta into the case state, emitting timeline
    /// milestones for changes Argus cares about.
    fn merge_live(&self, active: &mut ActiveCase, live: LiveAssessment, now: chrono::DateTime<Utc>) {
        active.state.summary = live.summary;

        if live.threat_level != active.state.threat_level {
            let detail = format!(
                "Threat level {:?} → {:?}.",
                active.state.threat_level, live.threat_level
            );
            active.state.threat_level = live.threat_level;
            active.state.push_event(now, TimelineKind::ThreatLevelChanged, detail);
        }

        // Locations: replace wholesale from the live view.
        active.state.locations = live
            .locations
            .into_iter()
            .map(|l| {
                let camera = self
                    .label_to_entity
                    .get(&l.camera_label)
                    .cloned()
                    .unwrap_or_else(|| l.camera_label.clone());
                LocationObservation {
                    camera,
                    label: l.camera_label,
                    activity: l.activity,
                    person_present: l.person_present,
                    last_seen: now,
                }
            })
            .collect();

        // Intruders: reconcile by stable id.
        for d in live.intruders {
            if let Some(existing) = active.state.intruder_mut(&d.id) {
                existing.confidence = d.confidence;
                existing.location = d.location;
                existing.activity = d.activity;
                // Don't let the fast loop clobber the firmer Opus descriptors.
                if !existing.identified {
                    existing.descriptors = d.descriptors;
                }
                existing.best_camera = d.best_camera_label;
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
                    id,
                    descriptors: d.descriptors,
                    confidence: d.confidence,
                    location: d.location,
                    activity: d.activity,
                    best_stills: Vec::new(),
                    identified: false,
                    dossier: None,
                    best_camera: d.best_camera_label,
                });
            }
        }
    }

    /// For each intruder whose best view improved, save a fresh still and run the
    /// Opus forensic pass.
    async fn upgrade_intruders(
        &mut self,
        active: &mut ActiveCase,
        stills: &HashMap<String, Vec<u8>>,
        now: chrono::DateTime<Utc>,
    ) {
        // Collect the work first (immutable scan), then mutate — keeps the
        // borrow checker happy and the Opus calls sequential (cost control).
        let mut jobs: Vec<(String, String, Vec<u8>)> = Vec::new(); // (id, camera_label, jpeg)
        for intruder in &active.state.intruders {
            let Some(label) = intruder.best_camera.clone() else {
                continue;
            };
            let Some(jpeg) = stills.get(&label) else {
                continue;
            };
            let last = active.still_conf.get(&intruder.id).copied();
            let improved = match last {
                None => true,
                Some(prev) => intruder.confidence >= prev + CONF_IMPROVE,
            };
            if improved {
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
                active.still_conf.insert(id.clone(), intruder.confidence);
            }
            active.state.push_event(
                now,
                TimelineKind::BestStillUpgraded,
                format!("Clearer still captured for {id}."),
            );

            // Opus forensic identification on that still.
            match self.identify(&label, &jpeg).await {
                Ok(ident) => {
                    if let Some(intruder) = active.state.intruder_mut(&id) {
                        intruder.descriptors = ident.descriptors;
                        intruder.dossier = Some(ident.dossier);
                        intruder.confidence = intruder.confidence.max(ident.confidence);
                        intruder.identified = true;
                    }
                    active.state.push_event(
                        now,
                        TimelineKind::IntruderIdentified,
                        format!("{id} identified: {}", ident.distinguishing_features),
                    );
                }
                Err(e) => warn!("opus identification failed for {id}: {e:#}"),
            }
        }
    }

    async fn identify(&mut self, label: &str, jpeg: &[u8]) -> Result<Identification> {
        let req = AssessRequest {
            seed: &self.seed,
            images: vec![ImageInput { label, jpeg }],
            instruction: "This is the clearest still of a detected intruder. Produce a firm \
                          forensic identification: physical descriptors, distinguishing features, \
                          and a short dossier paragraph for the security station. Anchor every \
                          claim on the image."
                .to_string(),
            schema: Some(identification_schema()),
            max_tokens: ID_MAX_TOKENS,
            reasoning: Reasoning::Deep,
        };
        let completion = self.opus.assess(&req).await.context("opus identification")?;
        self.record_usage(&completion.usage);
        completion.parse()
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

/// Ensure the subject counter is at least `n` (used when the model coins ids).
fn self_next_subject_at_least(active: &mut ActiveCase, n: u32) {
    if active.next_subject < n {
        active.next_subject = n;
    }
}
