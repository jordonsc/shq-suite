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
    /// What each camera currently sees (replaced each tick).
    pub locations: Vec<LocationObservation>,
    /// Detected intruders, reconciled by stable `id` across ticks.
    pub intruders: Vec<Intruder>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Current camera label.
    pub location: Option<String>,
    /// Current activity.
    pub activity: Option<String>,
    /// Ordered best→worst identifying stills.
    pub best_stills: Vec<StillRef>,
    /// True once the Opus forensic pass has produced firm descriptors.
    pub identified: bool,
    /// Richer forensic paragraph from the Opus pass (for the dossier).
    pub dossier: Option<String>,
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
    IntruderIdentified,
    BestStillUpgraded,
    ThreatLevelChanged,
    SecurityStationNotified,
    Standdown,
    Cleared,
}

impl CaseState {
    pub fn new(case_id: String, started_at: DateTime<Utc>) -> Self {
        Self {
            case_id,
            started_at,
            status: CaseStatus::Triggered,
            summary: "Alarm triggered; assessment starting.".to_string(),
            threat_level: ThreatLevel::Elevated,
            locations: Vec::new(),
            intruders: Vec::new(),
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
    pub location: Option<String>,
    pub activity: Option<String>,
    /// Which camera label currently has the clearest view of this person.
    pub best_camera_label: Option<String>,
}

/// The forensic (Opus) identification of one intruder's best still. Schema:
/// [`identification_schema`].
#[derive(Debug, Clone, Deserialize)]
pub struct Identification {
    pub descriptors: String,
    pub dossier: String,
    pub confidence: f32,
    pub distinguishing_features: String,
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
                        "location": { "type": ["string", "null"], "description": "Current camera label, or null." },
                        "activity": { "type": ["string", "null"], "description": "What they are doing, or null." },
                        "best_camera_label": { "type": ["string", "null"], "description": "Camera label with the clearest current view of this person, or null." }
                    },
                    "required": ["id", "descriptors", "confidence", "location", "activity", "best_camera_label"]
                }
            }
        },
        "required": ["summary", "threat_level", "person_detected", "locations", "intruders"]
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
            "distinguishing_features": { "type": "string", "description": "Tattoos, scars, logos, gait, carried items — anything that aids identification." }
        },
        "required": ["descriptors", "dossier", "confidence", "distinguishing_features"]
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
