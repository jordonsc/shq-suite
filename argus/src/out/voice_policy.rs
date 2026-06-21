//! Phase 3 — the **positive-only** voice policy gate.
//!
//! A pure function [`lines_for`] maps a [`TimelineEvent`] (+ `CaseState` context)
//! to zero or more [`SpokenLine`]s. The voice channel is deliberately one-sided:
//! it announces *wins* to unsettle an intruder (the breach, the intruder profile,
//! a detected weapon) and **never** verbalises a failure, gap, or uncertainty —
//! revealing those would undermine the intimidation.
//!
//! The gate is **structurally** incapable of speaking a negative: it is a
//! whitelist `match` over the closed [`TimelineKind`] vocabulary. Failure modes
//! (LLM uncertain, camera offline, PD send failed) are never *timeline events* in
//! the first place — they are logged by their channel and never reach this gate,
//! so there is no code path from a failure to a line.
//!
//! ## What it speaks (deliberately terse — the klaxon does the alerting)
//! - `case_opened` → "Security breach detected[, <location>]." (the klaxon line)
//! - `intruder_identified` → "Sending intruder profile to security responders."
//!   (the *profile itself* goes to PagerDuty, not over the speaker)
//! - `weapon_detected` → "Weapon detected! Intruder N armed with <weapon>."
//!
//! Everything else is silent: threat-level changes (announcing "critical" sounds
//! like panic), best-still upgrades, per-subject detection, acknowledgements,
//! standdown/cleared.
//!
//! ## Pacing (caller's responsibility)
//! This function is pure and emits at most one line per event. The wiring layer
//! ([`crate::out`]) queues lines onto a single voice worker so milestones don't
//! talk over each other; it also debounces by only routing *new* timeline events
//! (it diffs the timeline across `watch` updates). The gate itself holds no
//! state — pacing/debounce live in the consumer, not here.

use crate::case::{CaseState, TimelineEvent, TimelineKind};

/// A line Argus will speak via Overwatch `Verbalise`. Produced only by
/// [`lines_for`]; there is no constructor that yields a "failure" line.
#[derive(Debug, Clone, PartialEq)]
pub struct SpokenLine {
    /// The text handed to Overwatch's TTS.
    pub text: String,
    /// Per-line volume override (0.0–1.0). `None` = use the configured
    /// `voice_volume` default (the full/normal level).
    pub volume: Option<f32>,
}

impl SpokenLine {
    /// A line at the configured default voice volume.
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            volume: None,
        }
    }
}

/// Map a timeline event to zero or more spoken lines (in order).
///
/// `state` is the *current* `CaseState` the event belongs to, read to enrich a
/// line (the breach location, the intruder profile, an armed intruder's weapon).
/// Pure — no state held. An empty `Vec` means silent.
pub fn lines_for(event: &TimelineEvent, state: &CaseState) -> Vec<SpokenLine> {
    match event.kind {
        // ── Breach announcement (rides with the klaxon) ─────────────────────
        // `CaseOpened` fires it for a real `Alarm` case at open; `Escalated` fires
        // the SAME breach line when a gated `Investigate` case is promoted to
        // `Alarm` (the breach is announced retroactively at the promotion edge — the
        // first justified moment to speak). A GATED `Investigate` `CaseOpened` stays
        // SILENT (the double-check is silent until it escalates).
        TimelineKind::CaseOpened if state.gated() => Vec::new(),
        TimelineKind::CaseOpened | TimelineKind::Escalated => {
            // Prefer the alarm's trigger location (the zone that tripped it), then
            // any camera person-present fix; else a generic breach line.
            let location = state
                .trigger_location
                .as_deref()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .or_else(|| {
                    state
                        .locations
                        .iter()
                        .find(|l| l.person_present && !l.label.trim().is_empty())
                        .map(|l| l.label.trim())
                });
            match location {
                Some(l) => vec![SpokenLine::new(format!("Security breach detected in {l}."))],
                None => vec![SpokenLine::new("Security breach detected.")],
            }
        }
        // Verbalise the intruder PROFILE (the intimidation piece), then state the
        // profile was sent to responders (the dossier itself goes to PagerDuty).
        TimelineKind::IntruderIdentified => {
            let id = event.detail.split(" identified").next().unwrap_or("").trim();
            // Speak the CONCISE summary, not the full forensic paragraph (which is
            // far too long for TTS). Fall back to descriptors only if absent.
            let profile = state.persons.iter().find(|p| p.id == id).and_then(|p| {
                p.spoken_summary
                    .as_deref()
                    .or(Some(p.descriptors.as_str()))
                    .map(str::trim)
                    .filter(|d| !d.is_empty())
            });
            let mut out = Vec::new();
            if let Some(d) = profile {
                out.push(SpokenLine::new(format!("Intruder identified. {d}")));
            }
            out.push(SpokenLine::new(
                "Intruder profile sent to security responders.",
            ));
            out
        }
        // ── Intruder movement: terse room-to-room tracking ─────────────────
        TimelineKind::IntruderEnteredZone => {
            // Detail is "Intruder entered <zone>." — recover the zone and speak the
            // tracking line. Falls back to the whole detail if the prefix is absent.
            let zone = event
                .detail
                .trim()
                .strip_prefix("Intruder entered ")
                .unwrap_or(event.detail.trim())
                .trim_end_matches('.')
                .trim();
            if zone.is_empty() {
                Vec::new()
            } else {
                vec![SpokenLine::new(format!("Intruder in {zone}."))]
            }
        }
        TimelineKind::WeaponDetected => {
            // The event detail is "<id> is armed[: <weapon>]" — pull the subject id,
            // render it as "Intruder N", and name the weapon from current state.
            let id = event.detail.split(" is armed").next().unwrap_or("").trim();
            let number = id
                .rsplit('-')
                .next()
                .and_then(|s| s.parse::<u32>().ok())
                .map(|n| n.to_string())
                .unwrap_or_else(|| "1".to_string());
            let weapon = state
                .persons
                .iter()
                .find(|p| p.id == id)
                .and_then(|p| p.weapon.as_deref())
                .map(str::trim)
                .filter(|w| !w.is_empty());
            match weapon {
                Some(w) => vec![SpokenLine::new(format!(
                    "Weapon detected! Intruder {number} armed with {w}."
                ))],
                None => vec![SpokenLine::new(format!(
                    "Weapon detected! Intruder {number} is armed."
                ))],
            }
        }
        // ── Casualty: a person seen injured (once-off per person) ───────────
        // Detail is "<id> appears injured: <injury>." — recover the injury and
        // speak a terse casualty line; fall back to a generic line if absent.
        TimelineKind::InjuryDetected => {
            let injury = event
                .detail
                .split("appears injured:")
                .nth(1)
                .map(str::trim)
                .map(|s| s.trim_end_matches('.').trim())
                .filter(|s| !s.is_empty());
            match injury {
                Some(i) => vec![SpokenLine::new(format!("Casualty detected — {i}."))],
                None => vec![SpokenLine::new("Casualty detected — a person appears injured.")],
            }
        }
        // ── Malicious activity: a terse one-off line per distinct action ─────
        // Detail is the action string itself (e.g. "breaking into car").
        TimelineKind::MaliciousActivity => {
            let action = event.detail.trim().trim_end_matches('.').trim();
            if action.is_empty() {
                Vec::new()
            } else {
                vec![SpokenLine::new(format!("Detected: {action}."))]
            }
        }

        // ── Silent ──────────────────────────────────────────────────────────
        // Threat-level changes (announcing "critical" sounds like panic),
        // best-still upgrades, per-subject detection, security-station marker,
        // operator acknowledgement, and standdown/cleared all stay silent.
        TimelineKind::ThreatLevelChanged
        | TimelineKind::BestStillUpgraded
        | TimelineKind::SecurityStationNotified
        | TimelineKind::IntruderDetected
        | TimelineKind::Acknowledged
        | TimelineKind::Standdown
        | TimelineKind::Cleared => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::{CaseState, Person, TimelineKind};
    use chrono::Utc;

    fn ev(kind: TimelineKind, detail: &str) -> TimelineEvent {
        TimelineEvent {
            at: Utc::now(),
            kind,
            detail: detail.to_string(),
        }
    }

    fn case() -> CaseState {
        CaseState::new("case-test".to_string(), Utc::now(), crate::case::TriggerProfile::Alarm)
    }

    fn person(id: &str, descriptors: &str, armed: bool, weapon: Option<&str>) -> Person {
        Person {
            id: id.to_string(),
            descriptors: descriptors.to_string(),
            resident_confidence: 0.1,
            guest_confidence: 0.1,
            intruder_confidence: 0.9,
            id_score: 0.8,
            presence: 0.9,
            location: None,
            activity: None,
            armed,
            weapon_confidence: if armed { 0.9 } else { 0.0 },
            weapon: weapon.map(str::to_string),
            injury: None,
            in_duress: 0.0,
            best_stills: Vec::new(),
            identified: true,
            dossier: None,
            spoken_summary: None,
            best_camera: None,
        }
    }

    #[test]
    fn standdown_and_cleared_are_silent() {
        let s = case();
        assert!(lines_for(&ev(TimelineKind::Standdown, "disarmed"), &s).is_empty());
        assert!(lines_for(&ev(TimelineKind::Cleared, "cleared"), &s).is_empty());
    }

    #[test]
    fn intruder_detected_is_silent() {
        let s = case();
        assert!(lines_for(&ev(TimelineKind::IntruderDetected, "subject-1 appeared"), &s).is_empty());
    }

    #[test]
    fn intruder_identified_verbalises_profile_then_sent() {
        // The PROFILE is the intimidation piece (spoken), THEN a past-tense
        // "sent to responders" line.
        let mut s = case();
        s.persons
            .push(person("subject-1", "tall, dark hoodie", false, None));
        let lines = lines_for(&ev(TimelineKind::IntruderIdentified, "subject-1 identified: beard"), &s);
        assert_eq!(lines.len(), 2);
        // No spoken_summary yet → falls back to descriptors.
        assert_eq!(lines[0].text, "Intruder identified. tall, dark hoodie");
        assert_eq!(lines[1].text, "Intruder profile sent to security responders.");
    }

    #[test]
    fn intruder_identified_prefers_concise_spoken_summary() {
        let mut s = case();
        let mut i = person("subject-1", "a very long forensic paragraph that runs on and on", false, None);
        i.spoken_summary = Some("Caucasian male, forties, navy t-shirt, red beard.".to_string());
        s.persons.push(i);
        let lines = lines_for(&ev(TimelineKind::IntruderIdentified, "subject-1 identified: x"), &s);
        assert_eq!(lines[0].text, "Intruder identified. Caucasian male, forties, navy t-shirt, red beard.");
        assert!(!lines[0].text.contains("forensic paragraph"));
    }

    #[test]
    fn weapon_detected_names_intruder_and_weapon() {
        let mut s = case();
        s.persons
            .push(person("subject-1", "tall, dark hoodie", true, Some("kitchen knife")));
        let lines =
            lines_for(&ev(TimelineKind::WeaponDetected, "subject-1 is armed: kitchen knife"), &s);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Weapon detected! Intruder 1 armed with kitchen knife.");
    }

    #[test]
    fn case_opened_speaks_breach_with_trigger_location() {
        // Bare when no trigger location is known.
        let s = case();
        let bare = lines_for(&ev(TimelineKind::CaseOpened, "opened"), &s);
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].text, "Security breach detected.");
        // The trigger location (the zone that tripped the alarm) is named.
        let mut s2 = case();
        s2.trigger_location = Some("Garage".to_string());
        let located = lines_for(&ev(TimelineKind::CaseOpened, "opened"), &s2);
        assert_eq!(located[0].text, "Security breach detected in Garage.");
    }

    #[test]
    fn intruder_entered_zone_speaks_terse_tracking_line() {
        let s = case();
        let lines = lines_for(&ev(TimelineKind::IntruderEnteredZone, "Intruder entered Backyard."), &s);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Intruder in Backyard.");
    }

    #[test]
    fn intruder_entered_zone_tolerates_bare_detail() {
        // If the detail isn't the expected sentence, fall back to its content.
        let s = case();
        let lines = lines_for(&ev(TimelineKind::IntruderEnteredZone, "Kitchen"), &s);
        assert_eq!(lines[0].text, "Intruder in Kitchen.");
        // Empty detail stays silent rather than speaking "Intruder in .".
        assert!(lines_for(&ev(TimelineKind::IntruderEnteredZone, ""), &s).is_empty());
    }

    #[test]
    fn threat_change_and_best_still_are_silent() {
        let s = case();
        assert!(
            lines_for(&ev(TimelineKind::ThreatLevelChanged, "elevated → critical"), &s).is_empty(),
            "threat-level changes must be silent (no panic chatter)"
        );
        assert!(
            lines_for(&ev(TimelineKind::BestStillUpgraded, "clearer still"), &s).is_empty(),
            "best-still upgrades must be silent"
        );
    }

    #[test]
    fn injury_speaks_casualty_line() {
        let s = case();
        let lines = lines_for(
            &ev(TimelineKind::InjuryDetected, "subject-1 appears injured: stab wound."),
            &s,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Casualty detected — stab wound.");
        // A bare detail still speaks a generic casualty line.
        let generic = lines_for(&ev(TimelineKind::InjuryDetected, "subject-1"), &s);
        assert_eq!(generic[0].text, "Casualty detected — a person appears injured.");
    }

    #[test]
    fn malicious_activity_speaks_terse_line() {
        let s = case();
        let lines = lines_for(&ev(TimelineKind::MaliciousActivity, "breaking into car"), &s);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "Detected: breaking into car.");
        // Empty detail stays silent.
        assert!(lines_for(&ev(TimelineKind::MaliciousActivity, ""), &s).is_empty());
    }
}
