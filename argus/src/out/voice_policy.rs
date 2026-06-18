//! Phase 3 — the **positive-only** voice policy gate.
//!
//! A pure function [`line_for`] maps a [`TimelineEvent`] (+ `CaseState` context)
//! to `Some(SpokenLine)` ONLY for affirmative milestones, and `None` for
//! everything else. The voice channel is deliberately one-sided: it announces
//! *wins* to unsettle an intruder ("Intruder identified. Security station has
//! received your image.") and **never** verbalises a failure, gap, or
//! uncertainty — revealing those would undermine the intimidation.
//!
//! The gate is **structurally** incapable of speaking a negative: it is a
//! whitelist `match` over the closed [`TimelineKind`] vocabulary. The non-
//! affirmative kinds (`Standdown`, `Cleared`) and any future kind fall through
//! to `None`. Failure modes (LLM uncertain, camera offline, PD send failed) are
//! never *timeline events* in the first place — they are logged by their channel
//! and never reach this gate, so there is no code path from a failure to a line.
//!
//! `threat_level_changed` is the one conditional case: speak ONLY on an
//! escalation (info → elevated → critical), never on a de-escalation.
//!
//! ## Pacing (caller's responsibility)
//! This function is pure and emits at most one line per event. The wiring layer
//! ([`crate::out`]) queues lines onto a single voice worker so milestones don't
//! talk over each other; it also debounces by only routing *new* timeline events
//! (it diffs the timeline across `watch` updates). The gate itself holds no
//! state — pacing/debounce live in the consumer, not here.

use crate::case::{CaseState, ThreatLevel, TimelineEvent, TimelineKind};

/// A line Argus will speak via Overwatch `Verbalise`. Produced only by
/// [`line_for`]; there is no constructor that yields a "failure" line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpokenLine {
    /// The text handed to Overwatch's TTS.
    pub text: String,
}

impl SpokenLine {
    fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// Ordinal rank of a threat level, so we can tell escalation from de-escalation.
fn rank(level: ThreatLevel) -> u8 {
    match level {
        ThreatLevel::Info => 0,
        ThreatLevel::Elevated => 1,
        ThreatLevel::Critical => 2,
    }
}

/// Map a timeline event to an affirmative spoken line, or `None`.
///
/// `state` is the *current* `CaseState` the event belongs to (used to enrich a
/// line, e.g. append an intruder descriptor, and to read the current
/// `threat_level` for the escalation check). This function speaks for a
/// whitelisted closed set of [`TimelineKind`]s ONLY — `Standdown`, `Cleared`,
/// and any other kind return `None`.
pub fn line_for(event: &TimelineEvent, state: &CaseState) -> Option<SpokenLine> {
    match event.kind {
        // ── Affirmative milestones (the whitelist) ──────────────────────────
        TimelineKind::CaseOpened => {
            Some(SpokenLine::new("Intruder detected. Security protocol engaged."))
        }
        TimelineKind::IntruderIdentified => {
            // Optionally enrich with the most recently-identified intruder's
            // descriptor, if one is on record. Pure read of current state.
            let descriptor = state
                .intruders
                .iter()
                .rfind(|i| i.identified && !i.descriptors.trim().is_empty())
                .map(|i| i.descriptors.trim());
            match descriptor {
                Some(d) => Some(SpokenLine::new(format!("Intruder identified. {d}."))),
                None => Some(SpokenLine::new("Intruder identified.")),
            }
        }
        TimelineKind::BestStillUpgraded => Some(SpokenLine::new("Clear image captured.")),
        TimelineKind::SecurityStationNotified => Some(SpokenLine::new(
            "Security station has received your image and location.",
        )),

        // ── Conditional: only speak on a threat ESCALATION ──────────────────
        TimelineKind::ThreatLevelChanged => {
            // The event detail records the transition ("X → Y"); we re-derive the
            // direction from the new level vs the level encoded in the detail. To
            // stay a pure function with no prior-state access, we parse the arrow
            // form the engine writes ("<from> → <to>"). If we cannot determine an
            // escalation, stay silent (fail closed to None).
            if detail_is_escalation(&event.detail) {
                Some(SpokenLine::new(escalation_line(state.threat_level)))
            } else {
                None
            }
        }

        // ── Never speak: not affirmative, or a standdown/clear ──────────────
        // IntruderDetected is deliberately NOT spoken: the case-open line already
        // announced the protocol; per-subject detection chatter is left to the
        // HUD, and speaking it could reveal we are still counting people.
        // Acknowledged is an operator-side milestone (HA control surface); it is
        // not broadcast over the klaxon — stay silent (fail closed to None).
        TimelineKind::IntruderDetected
        | TimelineKind::Acknowledged
        | TimelineKind::Standdown
        | TimelineKind::Cleared => None,
    }
}

/// Spoken line for a threat escalation to `now`.
fn escalation_line(now: ThreatLevel) -> &'static str {
    match now {
        ThreatLevel::Critical => "Threat level critical.",
        ThreatLevel::Elevated => "Threat level elevated.",
        // An escalation that lands on Info should not happen, but if it does,
        // a neutral affirmative keeps the channel positive.
        ThreatLevel::Info => "Situation under assessment.",
    }
}

/// Decide whether a `ThreatLevelChanged` detail string describes an escalation.
///
/// The engine writes the detail as `"<from> threat → <to>"` style text containing
/// both the old and new snake_case levels. We extract the first and last level
/// token and compare ranks; an escalation is `rank(to) > rank(from)`. If we can't
/// find two distinct levels we fail closed (treat as NOT an escalation → silent).
fn detail_is_escalation(detail: &str) -> bool {
    let levels: Vec<ThreatLevel> = detail
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter_map(parse_level)
        .collect();
    match (levels.first(), levels.last()) {
        (Some(&from), Some(&to)) if levels.len() >= 2 => rank(to) > rank(from),
        _ => false,
    }
}

fn parse_level(tok: &str) -> Option<ThreatLevel> {
    match tok.to_ascii_lowercase().as_str() {
        "info" => Some(ThreatLevel::Info),
        "elevated" => Some(ThreatLevel::Elevated),
        "critical" => Some(ThreatLevel::Critical),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{CaseState, Intruder, TimelineKind};
    use chrono::Utc;

    fn ev(kind: TimelineKind, detail: &str) -> TimelineEvent {
        TimelineEvent {
            at: Utc::now(),
            kind,
            detail: detail.to_string(),
        }
    }

    fn case() -> CaseState {
        CaseState::new("case-test".to_string(), Utc::now())
    }

    #[test]
    fn standdown_and_cleared_are_silent() {
        let s = case();
        assert_eq!(line_for(&ev(TimelineKind::Standdown, "disarmed"), &s), None);
        assert_eq!(line_for(&ev(TimelineKind::Cleared, "cleared"), &s), None);
    }

    #[test]
    fn intruder_detected_is_silent() {
        let s = case();
        assert_eq!(
            line_for(&ev(TimelineKind::IntruderDetected, "subject-1 appeared"), &s),
            None
        );
    }

    #[test]
    fn intruder_identified_speaks() {
        let s = case();
        let line = line_for(&ev(TimelineKind::IntruderIdentified, "subject-1 firmed up"), &s);
        assert!(line.is_some(), "intruder_identified must speak");
        assert!(line.unwrap().text.starts_with("Intruder identified"));
    }

    #[test]
    fn intruder_identified_appends_descriptor() {
        let mut s = case();
        s.intruders.push(Intruder {
            id: "subject-1".to_string(),
            descriptors: "tall, dark hoodie".to_string(),
            confidence: 0.9,
            location: None,
            activity: None,
            best_stills: Vec::new(),
            identified: true,
            dossier: None,
            best_camera: None,
        });
        let line = line_for(&ev(TimelineKind::IntruderIdentified, ""), &s).unwrap();
        assert!(line.text.contains("tall, dark hoodie"));
    }

    #[test]
    fn case_opened_speaks() {
        let s = case();
        assert!(line_for(&ev(TimelineKind::CaseOpened, "opened"), &s).is_some());
    }

    #[test]
    fn threat_change_speaks_only_on_escalation() {
        let mut s = case();
        // Escalation: elevated → critical.
        s.threat_level = ThreatLevel::Critical;
        assert!(
            line_for(&ev(TimelineKind::ThreatLevelChanged, "elevated → critical"), &s).is_some(),
            "escalation must speak"
        );
        // De-escalation: critical → elevated → silent.
        s.threat_level = ThreatLevel::Elevated;
        assert_eq!(
            line_for(&ev(TimelineKind::ThreatLevelChanged, "critical → elevated"), &s),
            None,
            "de-escalation must be silent"
        );
    }
}
