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

/// Bumped if the on-disk / broadcast shape changes incompatibly. v2: the
/// intruder-centric model became persons-with-confidences (`intruders` →
/// `persons`, with resident/guest/intruder confidences, injury, in_duress) and
/// added `malicious_activity`.
pub const SCHEMA_VERSION: u32 = 2;

// ───────────────────────────── Trigger profiles ─────────────────────────────

/// Why Argus was woken, which selects the case's initial posture and — crucially
/// — **whether its outputs are gated** until Argus confirms a threat. Designed in
/// `specs/argus/07-trigger-profiles.md`. Two flows: `Alarm` (a confirmed breach —
/// full immediate outputs) and `Investigate` (a presumed-benign approach — a
/// gated, silent double-check that only escalates to `Alarm` on structured
/// danger).
///
/// `#[serde(default)]` to `Alarm` so an old journal (no `trigger_profile` field)
/// deserialises to today's full-posture behaviour. The retired `general` flow's
/// journals deserialise to `Investigate` via the serde alias (it was rebranded).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TriggerProfile {
    /// Confirmed intrusion (the house alarm) — full immediate outputs.
    #[default]
    Alarm,
    /// A presumed-benign approach (e.g. a person at the front door) — a gated,
    /// silent, shallow double-check that escalates to `Alarm` only on structured
    /// danger (model `critical`, an armed person, an injury, or duress), else a
    /// quiet standdown. (The retired `general` profile rebranded.)
    #[serde(alias = "general")]
    Investigate,
}

impl TriggerProfile {
    /// The threat level a fresh case of this profile opens at. `Alarm` assumes an
    /// intrusion in progress (`Elevated`); `Investigate` starts at `Info`.
    pub fn initial_threat(self) -> ThreatLevel {
        match self {
            TriggerProfile::Alarm => ThreatLevel::ThreatPresent,
            TriggerProfile::Investigate => ThreatLevel::Benign,
        }
    }
}

impl std::str::FromStr for TriggerProfile {
    type Err = String;

    /// Parse the snake_case profile name (for the `--profile` CLI override).
    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "alarm" => Ok(TriggerProfile::Alarm),
            "investigate" | "general" => Ok(TriggerProfile::Investigate),
            other => Err(format!(
                "unknown trigger profile {other:?} (expected alarm|investigate)"
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
    /// What each camera currently sees (replaced each tick).
    pub locations: Vec<LocationObservation>,
    /// People assessed this case, reconciled by stable `id` across ticks. Each
    /// carries resident/guest/intruder confidences — Argus cannot presume anyone
    /// is an intruder; a person whose intruder confidence dominates is a "subject
    /// of concern" (`Person::is_subject_of_concern`), which the voice/HUD/PD/
    /// timeline vocabulary calls an "intruder".
    #[serde(default)]
    pub persons: Vec<Person>,
    /// Current threat observations — weapons, aggression, residents in apparent
    /// danger, signs of forced entry. Free text; re-baselined on each full sweep,
    /// additively merged on motion-gated partial ticks.
    #[serde(default)]
    pub threats: Vec<String>,
    /// Terse malicious actions observed in the scene (e.g. "breaking into car",
    /// "smashed window"). Re-baselined on a full sweep, unioned on a partial tick
    /// (same rule as `threats`).
    #[serde(default)]
    pub malicious_activity: Vec<String>,
    /// Append-only milestones — the dossier + HUD ticker + voice/PD triggers.
    pub timeline: Vec<TimelineEvent>,
    /// The priority MAIN image for the HUD primary pane (the large primary view).
    /// Captured from the frames already grabbed this tick — NOT concern-gated, so
    /// the kiosk is never blank when there is ANY activity (even an ambiguous /
    /// resident-only frame). Priority-ranked + sticky-downward: a higher-rank
    /// category replaces a lower one, but a lower one never displaces a higher.
    /// `#[serde(default)]` so old journals (no field) still deserialise.
    #[serde(default)]
    pub main_still: Option<StillRef>,
    /// The category string for the HUD label of [`main_still`]: one of
    /// `"life_threatening"`, `"malicious"`, `"intruder"`, `"any"`. `#[serde(default)]`
    /// so old journals still deserialise.
    #[serde(default)]
    pub main_still_category: Option<String>,
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

/// Threat severity. Outcome-named so the model isn't mis-calibrated by a vague
/// "elevated"; `Ord` follows declaration order (`Benign < ThreatPresent <
/// LifeThreatening`) so the engine can ratchet UP and decay DOWN by comparison.
/// `benign` = no concern; `threat_present` = a genuine threat (an intruder, armed
/// or not); `life_threatening` = immediate danger to life (armed / duress / injury).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreatLevel {
    /// `alias` loads pre-0.33.0 journals (`info`/`elevated`/`critical`).
    #[serde(alias = "info")]
    Benign,
    #[serde(alias = "elevated")]
    ThreatPresent,
    #[serde(alias = "critical")]
    LifeThreatening,
}

impl ThreatLevel {
    /// PagerDuty Events v2 severity (Phase 3 maps this; PD's enum is fixed).
    #[allow(dead_code)]
    pub fn pagerduty_severity(self) -> &'static str {
        match self {
            ThreatLevel::Benign => "info",
            ThreatLevel::ThreatPresent => "warning",
            ThreatLevel::LifeThreatening => "critical",
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

/// Minimum intruder confidence — and dominance over the resident/guest reads —
/// for a [`Person`] to count as a "subject of concern" (an "intruder" in the
/// spoken/timeline vocabulary). Below this, or out-dominated by resident/guest,
/// the person is not treated as a threat by voice/HUD/PagerDuty/forensic-ID.
pub const CONCERN_INTRUDER_FLOOR: f32 = 0.34;

/// `in_duress` confidence at or above which a person counts as a structured
/// duress signal — enough, on its own, to make a person [`Person::warrants_attention`]
/// regardless of how they classify (a resident under coercion is still a casualty
/// to surface). A high bar: only a confident duress read forces it.
pub const DURESS_FLOOR: f32 = 0.75;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    /// Stable within the case (e.g. `subject-1`) so the HUD doesn't thrash and the
    /// same person is tracked across ticks.
    pub id: String,
    /// Physical descriptors (firmed up by the Opus forensic pass).
    pub descriptors: String,
    /// 0.0–1.0 likelihood this person is a RESIDENT.
    #[serde(default)]
    pub resident_confidence: f32,
    /// 0.0–1.0 likelihood this person is an invited/expected GUEST or delivery.
    #[serde(default)]
    pub guest_confidence: f32,
    /// 0.0–1.0 likelihood this person is an INTRUDER. The three confidences are
    /// independent (need not sum to 1); the dominant class drives `is_subject_of_concern`.
    #[serde(default)]
    pub intruder_confidence: f32,
    /// 0.0–1.0 frame-quality-for-identification of this person's latest view
    /// (face/build clear, in focus, close, well-lit). Drives which still feeds the
    /// forensic pass and when to re-profile on a clearer shot.
    #[serde(default)]
    pub id_score: f32,
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
    /// A visible injury (e.g. "bleeding", "stab wound", "gunshot wound",
    /// "unconscious", "possibly deceased"), else None. Latched (once seen it is
    /// kept) so a later occluded frame doesn't drop a casualty.
    #[serde(default)]
    pub injury: Option<String>,
    /// 0.0–1.0 confidence this person is under duress (coerced, restrained,
    /// threatened, distressed). Tracked as the MAX seen.
    #[serde(default)]
    pub in_duress: f32,
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

impl Person {
    /// Whether this person's dominant classification is INTRUDER (and above
    /// [`CONCERN_INTRUDER_FLOOR`]) — a "subject of concern". The spoken/timeline
    /// vocabulary calls such a person an "intruder"; voice/HUD/PagerDuty and the
    /// forensic-ID pass key on this so a clearly-resident or clearly-guest person
    /// is neither announced nor profiled. Ties break toward concern (caution).
    pub fn is_subject_of_concern(&self) -> bool {
        self.intruder_confidence >= CONCERN_INTRUDER_FLOOR
            && self.intruder_confidence >= self.resident_confidence
            && self.intruder_confidence >= self.guest_confidence
    }

    /// Whether this person warrants the full attention treatment — forensic ID +
    /// mugshot card, zone-movement tracking, and the priority MAIN image. This is
    /// `is_subject_of_concern` **OR any structured danger** (visibly armed, a
    /// visible injury, or a confident duress read). The key fix after the first
    /// live test: a person can read as a *resident* (high resident confidence) yet
    /// be ARMED — structured danger must override the classification so we still
    /// ID, card, and track them. (Benign residents/guests are still skipped.)
    pub fn warrants_attention(&self) -> bool {
        self.is_subject_of_concern()
            || self.armed
            || self.injury.as_deref().is_some_and(|i| !i.trim().is_empty())
            || self.in_duress >= DURESS_FLOOR
    }
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
    /// A person was seen to be injured (e.g. bleeding, a wound, unconscious).
    /// Once-off per person; speaks a casualty line and re-pages the security station.
    InjuryDetected,
    /// A malicious action was observed (e.g. "breaking into car", "smashed
    /// window"). Once-off per distinct action; speaks a terse line (voice-only).
    MaliciousActivity,
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
            locations: Vec::new(),
            persons: Vec::new(),
            threats: Vec::new(),
            malicious_activity: Vec::new(),
            timeline: Vec::new(),
            main_still: None,
            main_still_category: None,
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

    /// Find a mutable person by stable id.
    pub fn person_mut(&mut self, id: &str) -> Option<&mut Person> {
        self.persons.iter_mut().find(|p| p.id == id)
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
    pub persons: Vec<PersonDelta>,
    /// Weapons / threats / danger signs observed this tick (free text). Argus
    /// re-baselines `CaseState.threats` from this on a full sweep, unions on a
    /// partial tick.
    pub threats: Vec<String>,
    /// Terse malicious actions observed this tick (e.g. "breaking into car",
    /// "smashed window"). Re-baselined on a full sweep, unioned on a partial tick.
    #[serde(default)]
    pub malicious_activity: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LocationDelta {
    pub camera_label: String,
    pub activity: String,
    pub person_present: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PersonDelta {
    /// Reuse an existing id from the supplied roster, or coin the next
    /// `subject-N` for a genuinely new person.
    pub id: String,
    pub descriptors: String,
    /// Independent 0.0–1.0 likelihoods this person is a resident / guest / intruder.
    pub resident_confidence: f32,
    pub guest_confidence: f32,
    pub intruder_confidence: f32,
    /// 0.0–1.0 suitability of THIS frame for identification (face/build visible,
    /// in focus, close, well-lit). Drives best-still selection + when to (re)run
    /// the forensic pass.
    pub id_score: f32,
    pub location: Option<String>,
    pub activity: Option<String>,
    /// Which camera label currently has the clearest view of this person.
    pub best_camera_label: Option<String>,
    /// True if this person is visibly holding/carrying a weapon in the stills.
    pub armed: bool,
    /// The weapon if `armed` (e.g. "kitchen knife", "wooden stick"), else null.
    pub weapon: Option<String>,
    /// A visible injury on this person (e.g. "bleeding", "stab wound"), else null.
    pub injury: Option<String>,
    /// 0.0–1.0 confidence this person is under duress (coerced/restrained/threatened).
    pub in_duress: f32,
}

/// The forensic (Opus) identification of one subject's best still. Schema:
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
    /// A visible injury the forensic pass can confirm on the clearest frame, else null.
    pub injury: Option<String>,
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
            "threat_level": { "type": "string", "enum": ["benign", "threat_present", "life_threatening"] },
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
            "persons": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "id": { "type": "string", "description": "Reuse an id from the supplied roster for the same person; coin subject-N for a new one." },
                        "descriptors": { "type": "string", "description": "Build/clothing/hair/markings — only what the image supports." },
                        "resident_confidence": { "type": "number", "description": "0.0-1.0 likelihood this person is a RESIDENT named in the briefing. Only high when they clearly match a named resident; a mere resemblance is not a match." },
                        "guest_confidence": { "type": "number", "description": "0.0-1.0 likelihood this person is an invited/expected GUEST or delivery person." },
                        "intruder_confidence": { "type": "number", "description": "0.0-1.0 likelihood this person is an INTRUDER. Independent of the other two (they need not sum to 1). When uncertain, weight toward intruder." },
                        "id_score": { "type": "number", "description": "0.0-1.0: how good THIS frame is for IDENTIFYING the person — 1.0 = face and build clearly visible, in focus, close, well-lit; low = far away, blurred, back turned, heavily occluded, or dark. Judge the FRAME's usefulness for an ID." },
                        "location": { "type": ["string", "null"], "description": "Current camera label, or null." },
                        "activity": { "type": ["string", "null"], "description": "What they are doing, or null." },
                        "best_camera_label": { "type": ["string", "null"], "description": "Camera label with the clearest current view of this person, or null." },
                        "armed": { "type": "boolean", "description": "True ONLY if the person is actually holding a weapon (a knife or blade, firearm, bat, club, or axe) or is clearly wielding an object as a weapon. An ordinary object carried or used normally — a box, parcel, bag, phone, cup, tool, or umbrella — is NOT a weapon and must be false, even during an alarm; only a genuinely weapon-like ambiguous object (e.g. a partially hidden blade) warrants a hedged flag." },
                        "weapon": { "type": ["string", "null"], "description": "Name of the weapon if armed (e.g. \"kitchen knife\"); hedge if unsure (e.g. \"possible knife\", \"elongated object, possibly a blade\"); else null." },
                        "injury": { "type": ["string", "null"], "description": "A visible injury on this person (e.g. \"bleeding\", \"stab wound\", \"gunshot wound\", \"unconscious\", \"possibly deceased\"), else null." },
                        "in_duress": { "type": "number", "description": "0.0-1.0 confidence this person is under duress — coerced, restrained, threatened, or in visible distress. 0.0 if they appear free and calm." }
                    },
                    "required": ["id", "descriptors", "resident_confidence", "guest_confidence", "intruder_confidence", "id_score", "location", "activity", "best_camera_label", "armed", "weapon", "injury", "in_duress"]
                }
            },
            "threats": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Weapons, threatening objects, aggression, persons in apparent danger, or signs of forced entry visible this tick. Empty if none."
            },
            "malicious_activity": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Terse malicious actions visible this tick, e.g. \"breaking into car\", \"smashed window\", \"forcing a door\". Empty if none."
            }
        },
        "required": ["summary", "threat_level", "person_detected", "locations", "persons", "threats", "malicious_activity"]
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
            "armed": { "type": "boolean", "description": "True ONLY if the person is actually holding a weapon (a knife or blade, firearm, bat, club, or axe) or is clearly wielding an object as a weapon — this is the clearest frame, scrutinise the hands. An ordinary object carried or used normally — a box, parcel, bag, phone, cup, tool, or umbrella — is NOT a weapon and must be false, even during an alarm; only a genuinely weapon-like ambiguous object (e.g. a partially hidden blade) warrants a hedged flag." },
            "weapon": { "type": ["string", "null"], "description": "Name of the weapon if armed; hedge if unsure (e.g. \"possible knife\"); else null." },
            "injury": { "type": ["string", "null"], "description": "A visible injury you can confirm on this clearest frame (e.g. \"bleeding\", \"stab wound\", \"gunshot wound\", \"unconscious\", \"possibly deceased\"), else null." }
        },
        "required": ["descriptors", "dossier", "confidence", "distinguishing_features", "spoken_summary", "armed", "weapon", "injury"]
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
        // The legacy `intruders` field is unknown now (ignored); `persons` defaults.
        assert!(state.persons.is_empty());
    }

    /// A retired `general`-profile journal deserialises to the rebranded
    /// `Investigate` profile (serde alias), not an error.
    #[test]
    fn legacy_general_profile_maps_to_investigate() {
        let p: TriggerProfile = serde_json::from_str("\"general\"").unwrap();
        assert_eq!(p, TriggerProfile::Investigate);
        // And the canonical name still round-trips.
        let p2: TriggerProfile = serde_json::from_str("\"investigate\"").unwrap();
        assert_eq!(p2, TriggerProfile::Investigate);
    }

    /// A round-trip through serde preserves an explicit non-default profile, and a
    /// gated case reports `gated() == true`.
    #[test]
    fn profile_round_trips_and_drives_gating() {
        let mut state =
            CaseState::new("case-test".to_string(), Utc::now(), TriggerProfile::Investigate);
        assert_eq!(state.threat_level, ThreatLevel::Benign);
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
        assert_eq!(s.threat_level, ThreatLevel::ThreatPresent);
        assert_eq!(s.trigger_profile, TriggerProfile::Alarm);
        assert!(!s.gated());
    }
}
