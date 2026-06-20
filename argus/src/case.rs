//! The **`CaseState`** contract — the structured, evolving record of one alarm
//! episode. This is the central interface of the whole project: the assessment
//! loop (Phase 2) produces it; offsite replication (2a), voice/PagerDuty (3) and
//! the kiosk HUD (4) consume it. No downstream phase calls the LLM or HA — they
//! render `CaseState`.
//!
//! Two families of types live here:
//! - **`CaseState`** and friends — the durable, broadcast record Argus owns.
//! - **`LiveAssessment` / `Identification`** — the structured *LLM outputs*, with
//!   their JSON schemas (`live_assessment_schema()` / `identification_schema()`)
//!   used as `output_config.format` so the model returns parseable JSON. These
//!   are deltas Argus merges into `CaseState`; Argus — not the LLM — derives the
//!   `timeline` by diffing state, so Phase 3 gets a controlled event vocabulary.
//!
//! The on-disk **case dir** (`CaseDir`) is the durable journal AND the upload
//! queue Phase 2a replicates: one object per event/still, atomic writes, so any
//! subset that reached disk (or offsite) is a coherent partial case.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Bumped if the on-disk / broadcast shape changes incompatibly.
pub const SCHEMA_VERSION: u32 = 1;

// ───────────────────────────── Trigger profiles ─────────────────────────────

/// Why Argus was woken, which selects the case's initial posture and — crucially
/// — **whether its outputs are gated** until Argus confirms a threat. Designed in
/// `specs/argus/07-trigger-profiles.md`. **Phase 4a lays the rails only**: every
/// real trigger today is the house alarm → `Alarm`, so the softer profiles exist
/// but are not yet exercised, and the gate is dormant.
///
/// `#[serde(default)]` to `Alarm` so an old journal (no `trigger_profile` field)
/// deserialises to today's full-posture behaviour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TriggerProfile {
    /// Confirmed intrusion (the house alarm) — full immediate outputs. Today's
    /// behaviour; the others are added beside it.
    #[default]
    Alarm,
    /// Perimeter activity while secured — a gated, silent assessment that only
    /// escalates to outputs if Argus concludes a real threat (Phase 4b).
    Investigate,
    /// A benign doorstep approach — a gated, fast/shallow triage (Phase 4b).
    General,
}

impl TriggerProfile {
    /// The threat level a fresh case of this profile opens at. `Alarm` assumes an
    /// intrusion in progress (`Elevated`); the gated profiles start at `Info`.
    pub fn initial_threat(self) -> ThreatLevel {
        match self {
            TriggerProfile::Alarm => ThreatLevel::Elevated,
            TriggerProfile::Investigate | TriggerProfile::General => ThreatLevel::Info,
        }
    }
}

// ───────────────────────────── Investigate smart-alarm outcome ──────────────

/// The smart-alarm classification the engine derives each tick for an active
/// `Investigate` case — the "what is this perimeter activity?" verdict that drives
/// the audible investigate loop (announce → assess → resolve). Distinct from
/// `ThreatLevel` (severity) and `TriggerProfile` (posture): it is the *decision*.
///
/// - `Threat`/`Intruder` resolve to **promote** (full `Alarm` + real-alarm trip,
///   subject to the ≥2-tick persistence gate).
/// - `Visitor`/`FalseAlarm` resolve to a **quiet stand down** (no alarm).
/// - `Undetermined` keeps assessing until the investigate deadline, then stands
///   down (treated as benign).
///
/// `#[serde(rename_all="snake_case")]` for the journal; Default `Undetermined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InvestigateOutcome {
    /// A new (non-resident) person AND residents are absent OR present-but-unaware.
    Intruder,
    /// A weapon / ski-mask / face-concealment on a new (non-resident) person.
    Threat,
    /// A new person BUT residents are present and visibly engaging them.
    Visitor,
    /// The person who tripped it is actually a resident — no genuine stranger.
    FalseAlarm,
    /// Not enough signal yet; keep assessing.
    #[default]
    Undetermined,
}

/// The model's read of whether the residents are present and AWARE of / engaging
/// the newcomer — the signal that separates a `Visitor` (engaged) from an
/// `Intruder` (absent or oblivious). Optional on the live assessment; only the
/// `Investigate` outcome classifier consumes it. Default `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ResidentAwareness {
    /// No residents visible / present in the scene.
    ResidentsAbsent,
    /// Residents are present but oblivious to the newcomer (not interacting).
    ResidentsPresentUnaware,
    /// Residents are present AND engaging the newcomer (waving, talking, relaxed).
    ResidentsPresentEngaging,
    /// Can't tell from this frame.
    #[default]
    Unknown,
}

impl std::str::FromStr for TriggerProfile {
    type Err = String;

    /// Parse the snake_case profile name (for the `--profile` CLI override).
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "alarm" => Ok(TriggerProfile::Alarm),
            "investigate" => Ok(TriggerProfile::Investigate),
            "general" => Ok(TriggerProfile::General),
            other => Err(format!(
                "unknown trigger profile {other:?} (expected alarm|investigate|general)"
            )),
        }
    }
}

// ───────────────────────────── CaseState (the contract) ─────────────────────

/// The single source of truth for an alarm episode. Serialised to the broadcast
/// channel (Phases 3/4) and journalled to disk (Phase 2a replicates it).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseState {
    /// Stable per alarm episode — the PagerDuty dedup key and the S3 case prefix.
    pub case_id: String,
    pub started_at: DateTime<Utc>,
    pub status: CaseStatus,
    /// One-line human situation summary (latest), from the live model.
    pub summary: String,
    pub threat_level: ThreatLevel,
    /// How the case opened (which trigger source woke Argus). Drives the initial
    /// threat + posture. `#[serde(default)]` → `Alarm` for back-compat with
    /// pre-4a journals.
    #[serde(default)]
    pub trigger_profile: TriggerProfile,
    /// The case's CURRENT posture. Equals `trigger_profile` at open; Phase 4b's
    /// promotion raises a gated case to `Alarm` (escalation only ever moves up).
    /// The output gate keys off THIS, not `trigger_profile`.
    #[serde(default)]
    pub effective_profile: TriggerProfile,
    /// Human location that tripped the alarm (e.g. "Garage", "Entry"), read once
    /// at case open from the configured trigger-location entity. Drives the spoken
    /// breach line. `None` if no such entity is configured / set.
    #[serde(default)]
    pub trigger_location: Option<String>,
    /// For an `Investigate` case: the engine's latest derived smart-alarm
    /// classification (`classify_investigate`) — journalled so the HUD / record
    /// shows the verdict. Ignored (left `Undetermined`) for the other profiles.
    #[serde(default)]
    pub investigate_outcome: InvestigateOutcome,
    /// What each camera currently sees (replaced each tick).
    pub locations: Vec<LocationObservation>,
    /// Detected intruders, reconciled by stable `id` across ticks.
    pub intruders: Vec<Intruder>,
    /// Current threat observations — weapons, aggression, residents in apparent
    /// danger, signs of forced entry. Free text; re-baselined on each full sweep,
    /// additively merged on motion-gated partial ticks.
    #[serde(default)]
    pub threats: Vec<String>,
    /// Append-only milestones — the dossier + HUD ticker + voice/PD triggers.
    pub timeline: Vec<TimelineEvent>,
    pub updated_at: DateTime<Utc>,
    pub schema_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    /// Alarm just fired; first assessment not yet landed.
    Triggered,
    /// The live loop is running and the case is evolving.
    Assessing,
    /// Alarm disarmed; loop stopping, final state being flushed.
    Standdown,
    /// Terminal — final state flushed.
    Cleared,
}

/// Threat severity. `Ord` follows declaration order (`Info < Elevated < Critical`)
/// so the engine can ratchet UP and decay DOWN by simple comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreatLevel {
    Info,
    Elevated,
    Critical,
}

impl ThreatLevel {
    /// PagerDuty Events v2 severity (Phase 3 maps this).
    #[allow(dead_code)]
    pub fn pagerduty_severity(self) -> &'static str {
        match self {
            ThreatLevel::Info => "info",
            ThreatLevel::Elevated => "warning",
            ThreatLevel::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocationObservation {
    /// HA camera entity id.
    pub camera: String,
    /// Human label.
    pub label: String,
    /// What is happening in that view (free text from the model).
    pub activity: String,
    pub person_present: bool,
    pub last_seen: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Intruder {
    /// Stable within the case (e.g. `subject-1`) so the HUD doesn't thrash.
    pub id: String,
    /// Physical descriptors (firmed up by the Opus forensic pass).
    pub descriptors: String,
    /// 0.0–1.0 identification confidence (latest).
    pub confidence: f32,
    /// 0.0–1.0 frame-quality-for-identification of this intruder's latest view
    /// (face/build clear, in focus, close, well-lit). Drives which still feeds the
    /// forensic pass and when to re-profile on a clearer shot. Distinct from
    /// `confidence`.
    #[serde(default)]
    pub id_quality: f32,
    /// Current camera label.
    pub location: Option<String>,
    /// Current activity.
    pub activity: Option<String>,
    /// True once this person has been seen visibly armed / carrying a weapon.
    /// Latched on (only the forensic pass or a clearer view firms it up; a single
    /// occluded frame should not clear a weapon already established).
    #[serde(default)]
    pub armed: bool,
    /// The weapon if `armed` (e.g. "kitchen knife", "wooden stick"), else None.
    #[serde(default)]
    pub weapon: Option<String>,
    /// Ordered best→worst identifying stills.
    pub best_stills: Vec<StillRef>,
    /// True once the Opus forensic pass has produced firm descriptors.
    pub identified: bool,
    /// Richer forensic paragraph from the Opus pass (for the dossier).
    pub dossier: Option<String>,
    /// A short one-sentence description for the spoken announcement (from the
    /// forensic pass). The verbalised profile uses this, not the full descriptors.
    #[serde(default)]
    pub spoken_summary: Option<String>,
    /// Camera label the live loop currently reports as the best view (drives
    /// still capture + the Opus pass). Transient; not part of the public record
    /// downstream renders, but kept on the struct for reconciliation.
    #[serde(default)]
    pub best_camera: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StillRef {
    /// File stem under `stills/` (Phase 4 serves `/stills/<id>.jpg`).
    pub id: String,
    pub camera: String,
    pub captured_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEvent {
    pub at: DateTime<Utc>,
    pub kind: TimelineKind,
    pub detail: String,
}

/// The closed milestone vocabulary Phase 3 maps to voice lines / PagerDuty
/// updates and Phase 4 renders in the ticker. Derived by Argus from state
/// diffs — the LLM does not emit these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineKind {
    CaseOpened,
    IntruderDetected,
    /// An intruder was seen in a ZONE (room/area) not yet announced this case —
    /// the intruder has moved into fresh territory. Drives a terse spoken
    /// "Intruder in <area>." line. The alarm's initial trigger location is
    /// excluded (it is seeded as already-announced at case open). Zone-level
    /// dedup: announced once per zone regardless of how many intruders enter it.
    IntruderEnteredZone,
    IntruderIdentified,
    /// A person was seen to be armed / a weapon or threatening object appeared.
    /// Material milestone: re-pages the security station and speaks a firm line.
    WeaponDetected,
    /// A gated (`Investigate`/`General`) case was **promoted** to `Alarm` — Argus
    /// concluded a real threat. From this milestone the outputs ungate and flow
    /// (the gate in `out::route_event` lets `Escalated` itself through). **Phase
    /// 4a defines the variant only**; the promotion that emits it is Phase 4b.
    Escalated,
    BestStillUpgraded,
    ThreatLevelChanged,
    SecurityStationNotified,
    /// An operator acknowledged the active case from the HA control surface
    /// (Phase 5 `/control` WS). Additive milestone — does not change the case
    /// status; the case keeps assessing.
    Acknowledged,
    Standdown,
    Cleared,
}

impl CaseState {
    /// Open a fresh case for the given trigger `profile`. The profile selects the
    /// initial `threat_level` (`Alarm` → `Elevated`; the gated profiles → `Info`)
    /// and seeds both `trigger_profile` and `effective_profile`.
    pub fn new(case_id: String, started_at: DateTime<Utc>, profile: TriggerProfile) -> Self {
        Self {
            case_id,
            started_at,
            status: CaseStatus::Triggered,
            summary: "Alarm triggered; assessment starting.".to_string(),
            threat_level: profile.initial_threat(),
            trigger_profile: profile,
            effective_profile: profile,
            trigger_location: None,
            investigate_outcome: InvestigateOutcome::Undetermined,
            locations: Vec::new(),
            intruders: Vec::new(),
            threats: Vec::new(),
            timeline: Vec::new(),
            updated_at: started_at,
            schema_version: SCHEMA_VERSION,
        }
    }

    /// Append a timeline milestone and bump `updated_at`.
    pub fn push_event(&mut self, at: DateTime<Utc>, kind: TimelineKind, detail: impl Into<String>) {
        self.timeline.push(TimelineEvent {
            at,
            kind,
            detail: detail.into(),
        });
        self.updated_at = at;
    }

    /// Find a mutable intruder by stable id.
    pub fn intruder_mut(&mut self, id: &str) -> Option<&mut Intruder> {
        self.intruders.iter_mut().find(|i| i.id == id)
    }

    /// True while the case's outputs are **gated** — its `effective_profile` is a
    /// softer (non-`Alarm`) posture, so voice/klaxon/PagerDuty/kiosk-takeover are
    /// suppressed until a Phase-4b promotion raises it to `Alarm`. An `Alarm` case
    /// (the only kind that opens today) is never gated.
    pub fn gated(&self) -> bool {
        self.effective_profile != TriggerProfile::Alarm
    }
}

// ───────────────────────────── LLM structured outputs ───────────────────────

/// The live (Sonnet) what/where assessment for one tick — a delta Argus merges
/// into `CaseState`. Schema: [`live_assessment_schema`].
#[derive(Debug, Clone, Deserialize)]
pub struct LiveAssessment {
    pub summary: String,
    pub threat_level: ThreatLevel,
    #[allow(dead_code)]
    pub person_detected: bool,
    pub locations: Vec<LocationDelta>,
    pub intruders: Vec<IntruderDelta>,
    /// Weapons / threats / danger signs observed this tick (free text). Argus
    /// re-baselines `CaseState.threats` from this on a full sweep, unions on a
    /// partial tick.
    pub threats: Vec<String>,
    /// Whether residents are present and ENGAGING the newcomer (the `Investigate`
    /// smart-alarm signal — separates a welcomed visitor from an unnoticed/absent-
    /// resident intruder). Optional: the model may omit it (older schema / non-
    /// investigate runs) → `None`, which the classifier reads as `Unknown`. The
    /// non-investigate profiles ignore it entirely.
    #[serde(default)]
    pub resident_awareness: Option<ResidentAwareness>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocationDelta {
    pub camera_label: String,
    pub activity: String,
    pub person_present: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IntruderDelta {
    /// Reuse an existing id from the supplied roster, or coin the next
    /// `subject-N` for a genuinely new person.
    pub id: String,
    pub descriptors: String,
    pub confidence: f32,
    /// 0.0–1.0 suitability of THIS frame for identification (face/build visible,
    /// in focus, close, well-lit) — distinct from `confidence`. Drives best-still
    /// selection + when to (re)run the forensic pass.
    pub id_quality: f32,
    pub location: Option<String>,
    pub activity: Option<String>,
    /// Which camera label currently has the clearest view of this person.
    pub best_camera_label: Option<String>,
    /// True if this person is visibly holding/carrying a weapon in the stills.
    pub armed: bool,
    /// The weapon if `armed` (e.g. "kitchen knife", "wooden stick"), else null.
    pub weapon: Option<String>,
}

/// The forensic (Opus) identification of one intruder's best still. Schema:
/// [`identification_schema`].
#[derive(Debug, Clone, Deserialize)]
pub struct Identification {
    pub descriptors: String,
    pub dossier: String,
    pub confidence: f32,
    pub distinguishing_features: String,
    /// A SHORT one-sentence description for the SPOKEN security announcement
    /// (build + clothing + one standout feature). The full `descriptors` stay on
    /// the record / PagerDuty; only this is verbalised.
    pub spoken_summary: String,
    /// Whether the forensic pass judges this person to be armed.
    pub armed: bool,
    /// The weapon if `armed`, else null.
    pub weapon: Option<String>,
}

/// JSON schema for the live assessment, used as `output_config.format`.
///
/// Structured-output rules (per the claude-api skill): every object has
/// `additionalProperties: false` and lists every property in `required`;
/// "optional" fields are nullable (`["string","null"]`) but still required; no
/// numeric/string constraints (`minimum`/`maxLength`) are permitted.
pub fn live_assessment_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "summary": { "type": "string", "description": "One sentence: who is present, what they are doing, where. If no person, say so." },
            "threat_level": { "type": "string", "enum": ["info", "elevated", "critical"] },
            "person_detected": { "type": "boolean" },
            "locations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "camera_label": { "type": "string", "description": "Exact label of the camera, as supplied." },
                        "activity": { "type": "string", "description": "What is happening in this view." },
                        "person_present": { "type": "boolean" }
                    },
                    "required": ["camera_label", "activity", "person_present"]
                }
            },
            "intruders": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string", "description": "Reuse an id from the supplied roster for the same person; coin subject-N for a new one." },
                        "descriptors": { "type": "string", "description": "Build/clothing/hair/markings — only what the image supports." },
                        "confidence": { "type": "number", "description": "0.0-1.0 confidence this is a real, identifiable person." },
                        "id_quality": { "type": "number", "description": "0.0-1.0: how good THIS frame is for IDENTIFYING the person — 1.0 = face and build clearly visible, in focus, close, well-lit; low = far away, blurred, back turned, heavily occluded, or dark. Judge the FRAME's usefulness for an ID, separately from `confidence`." },
                        "location": { "type": ["string", "null"], "description": "Current camera label, or null." },
                        "activity": { "type": ["string", "null"], "description": "What they are doing, or null." },
                        "best_camera_label": { "type": ["string", "null"], "description": "Camera label with the clearest current view of this person, or null." },
                        "armed": { "type": "boolean", "description": "True if holding/carrying a weapon OR an object plausibly consistent with one (blade, firearm, bat, stick, tool, or an elongated/pointed/metallic object). Lean toward flagging during an active intrusion; only a clearly-recognised benign object (phone, remote, cup) is not a weapon." },
                        "weapon": { "type": ["string", "null"], "description": "Name of the weapon if armed (e.g. \"kitchen knife\"); hedge if unsure (e.g. \"possible knife\", \"elongated object, possibly a blade\"); else null." }
                    },
                    "required": ["id", "descriptors", "confidence", "id_quality", "location", "activity", "best_camera_label", "armed", "weapon"]
                }
            },
            "threats": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Weapons, threatening objects, aggression, residents in apparent danger, or signs of forced entry visible this tick. Empty if none."
            },
            "resident_awareness": {
                "type": "string",
                "enum": ["residents_absent", "residents_present_unaware", "residents_present_engaging", "unknown"],
                "description": "PERIMETER-SECURITY signal: are the residents present and ENGAGING any newcomer? 'residents_present_engaging' = a resident is visibly interacting with the newcomer — waving, talking, walking together, relaxed close proximity (a welcomed guest). 'residents_present_unaware' = a resident is in view but oblivious / not interacting (e.g. indoors, back turned, unaware someone has approached). 'residents_absent' = NO resident is present in the scene at all (a lone newcomer). 'unknown' = you cannot tell from this frame. Judge engagement generously toward 'engaging' ONLY when the interaction is genuinely relaxed and mutual; a stranger alone, or residents who plainly haven't noticed them, is NOT engaging."
            }
        },
        "required": ["summary", "threat_level", "person_detected", "locations", "intruders", "threats", "resident_awareness"]
    })
}

/// JSON schema for the Opus forensic identification, used as `output_config.format`.
pub fn identification_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "descriptors": { "type": "string", "description": "Firm physical descriptors anchored on the best still." },
            "dossier": { "type": "string", "description": "A short forensic paragraph for the security-station dossier." },
            "confidence": { "type": "number", "description": "0.0-1.0 confidence in this identification." },
            "distinguishing_features": { "type": "string", "description": "Tattoos, scars, logos, gait, carried items — anything that aids identification." },
            "spoken_summary": { "type": "string", "description": "A SHORT single sentence (max ~18 words) for a spoken security announcement: sex, approx age, build, key clothing, and one standout feature. No preamble. E.g. 'Caucasian male, forties, navy t-shirt, grey track pants, full red beard.'" },
            "armed": { "type": "boolean", "description": "True if holding/carrying a weapon OR an object plausibly consistent with one (blade, firearm, bat, stick, tool, elongated/pointed/metallic object) — this is the clearest frame, scrutinise the hands. Only a clearly-recognised benign object (phone, remote, cup) is not a weapon." },
            "weapon": { "type": ["string", "null"], "description": "Name of the weapon if armed; hedge if unsure (e.g. \"possible knife\"); else null." }
        },
        "required": ["descriptors", "dossier", "confidence", "distinguishing_features", "spoken_summary", "armed", "weapon"]
    })
}

// ───────────────────────────── Case dir (journal = upload queue) ─────────────

/// One case's on-disk directory. The durable audit journal AND the upload queue
/// Phase 2a replicates to S3: one object per event/still, atomic writes (write
/// `.tmp` then rename), sequence-numbered events for deterministic ordering.
///
/// Layout (mirrors the Phase 2a S3 layout 1:1):
/// ```text
/// <data>/cases/<case_id>/
///   manifest.json           # case_id, started_at, schema version, alarm entity
///   events/000001.json      # one full CaseState snapshot per mutation
///   stills/<still_id>.jpg    # best stills
///   state.json              # latest snapshot (overwritten)
///   dossier.json            # final dossier on standdown
/// ```
pub struct CaseDir {
    root: PathBuf,
    event_seq: u64,
}

impl CaseDir {
    /// Create `cases/<case_id>/{events,stills}` under the data dir and write the
    /// manifest. `base` is typically `~/.local/share/argus`.
    pub fn create(base: &Path, case_id: &str, started_at: DateTime<Utc>, alarm_entity: &str) -> Result<Self> {
        let root = base.join("cases").join(case_id);
        std::fs::create_dir_all(root.join("events")).context("creating events dir")?;
        std::fs::create_dir_all(root.join("stills")).context("creating stills dir")?;
        let dir = Self { root, event_seq: 0 };
        dir.write_json(
            "manifest.json",
            &json!({
                "case_id": case_id,
                "started_at": started_at,
                "schema_version": SCHEMA_VERSION,
                "alarm_entity": alarm_entity,
            }),
        )?;
        Ok(dir)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Append a full `CaseState` snapshot as the next sequence-numbered event
    /// object, and refresh `state.json`. Object-per-event is the key 2a choice:
    /// every PUT is atomic and any prefix is a coherent partial case.
    pub fn append_event(&mut self, state: &CaseState) -> Result<()> {
        self.event_seq += 1;
        let name = format!("events/{:06}.json", self.event_seq);
        self.write_json(&name, state)?;
        self.write_json("state.json", state)?;
        Ok(())
    }

    /// Persist a still's JPEG bytes under `stills/<still_id>.jpg`.
    pub fn save_still(&self, still_id: &str, jpeg: &[u8]) -> Result<PathBuf> {
        let rel = format!("stills/{still_id}.jpg");
        let path = self.root.join(&rel);
        let tmp = path.with_extension("jpg.tmp");
        std::fs::write(&tmp, jpeg).with_context(|| format!("writing {rel}"))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("renaming {rel}"))?;
        Ok(path)
    }

    /// Write the final dossier on standdown.
    pub fn write_dossier(&self, state: &CaseState) -> Result<()> {
        self.write_json("dossier.json", state)
    }

    /// Atomic JSON write (`.tmp` then rename) so a 2a uploader never reads a
    /// half-written object.
    fn write_json<T: Serialize>(&self, rel: &str, value: &T) -> Result<()> {
        let path = self.root.join(rel);
        let tmp = path.with_extension("tmp");
        let bytes = serde_json::to_vec_pretty(value).context("serialising case json")?;
        std::fs::write(&tmp, &bytes).with_context(|| format!("writing {rel}"))?;
        std::fs::rename(&tmp, &path).with_context(|| format!("renaming {rel}"))?;
        Ok(())
    }
}

/// Default case-storage base: `~/.local/share/argus` (via `ProjectDirs`).
pub fn default_case_base() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "argus")
        .context("cannot determine data directory")?;
    Ok(dirs.data_dir().to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pre-4a journal has NO `trigger_profile`/`effective_profile` fields. It
    /// must deserialise with BOTH defaulting to `Alarm` — i.e. an existing case
    /// behaves exactly as in 0.27.0 (full-posture, never gated).
    #[test]
    fn old_journal_without_profile_deserialises_to_alarm() {
        let now = Utc::now();
        let json = json!({
            "case_id": "case-20260101T000000Z",
            "started_at": now,
            "status": "triggered",
            "summary": "Alarm triggered; assessment starting.",
            "threat_level": "elevated",
            "locations": [],
            "intruders": [],
            "timeline": [],
            "updated_at": now,
            "schema_version": SCHEMA_VERSION,
        });
        let state: CaseState =
            serde_json::from_value(json).expect("legacy journal must still deserialise");
        assert_eq!(state.trigger_profile, TriggerProfile::Alarm);
        assert_eq!(state.effective_profile, TriggerProfile::Alarm);
        assert!(!state.gated(), "an Alarm case is never gated");
    }

    /// A round-trip through serde preserves an explicit non-default profile, and a
    /// gated case reports `gated() == true`.
    #[test]
    fn profile_round_trips_and_drives_gating() {
        let mut state =
            CaseState::new("case-test".to_string(), Utc::now(), TriggerProfile::Investigate);
        assert_eq!(state.threat_level, ThreatLevel::Info);
        assert!(state.gated());

        let s = serde_json::to_string(&state).unwrap();
        // The snake_case rename must be on the wire.
        assert!(s.contains("\"trigger_profile\":\"investigate\""));
        let back: CaseState = serde_json::from_str(&s).unwrap();
        assert_eq!(back.trigger_profile, TriggerProfile::Investigate);
        assert_eq!(back.effective_profile, TriggerProfile::Investigate);
        assert!(back.gated());

        // Promotion to Alarm (the Phase-4b posture) ungates.
        state.effective_profile = TriggerProfile::Alarm;
        assert!(!state.gated());
    }

    /// `Alarm` opens at `Elevated` (today's behaviour); the gated profiles at `Info`.
    #[test]
    fn alarm_profile_opens_elevated() {
        let s = CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Alarm);
        assert_eq!(s.threat_level, ThreatLevel::Elevated);
        assert_eq!(s.trigger_profile, TriggerProfile::Alarm);
        assert!(!s.gated());
    }
}
