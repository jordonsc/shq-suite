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

/// Map a timeline event to zero or more spoken lines (in order).
///
/// `state` is the *current* `CaseState` the event belongs to, read to enrich a
/// line (the breach location, the intruder profile, an armed intruder's weapon).
/// Pure — no state held. An empty `Vec` means silent.
pub fn lines_for(event: &TimelineEvent, state: &CaseState) -> Vec<SpokenLine> {
    match event.kind {
        // ── Breach announcement (rides with the klaxon) ─────────────────────
        TimelineKind::CaseOpened => {
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
            let profile = state.intruders.iter().find(|i| i.id == id).and_then(|i| {
                i.spoken_summary
                    .as_deref()
                    .or(Some(i.descriptors.as_str()))
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
                .intruders
                .iter()
                .find(|i| i.id == id)
                .and_then(|i| i.weapon.as_deref())
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

    fn intruder(id: &str, descriptors: &str, armed: bool, weapon: Option<&str>) -> Intruder {
        Intruder {
            id: id.to_string(),
            descriptors: descriptors.to_string(),
            confidence: 0.9,
            location: None,
            activity: None,
            armed,
            weapon: weapon.map(str::to_string),
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
        s.intruders
            .push(intruder("subject-1", "tall, dark hoodie", false, None));
        let lines = lines_for(&ev(TimelineKind::IntruderIdentified, "subject-1 identified: beard"), &s);
        assert_eq!(lines.len(), 2);
        // No spoken_summary yet → falls back to descriptors.
        assert_eq!(lines[0].text, "Intruder identified. tall, dark hoodie");
        assert_eq!(lines[1].text, "Intruder profile sent to security responders.");
    }

    #[test]
    fn intruder_identified_prefers_concise_spoken_summary() {
        let mut s = case();
        let mut i = intruder("subject-1", "a very long forensic paragraph that runs on and on", false, None);
        i.spoken_summary = Some("Caucasian male, forties, navy t-shirt, red beard.".to_string());
        s.intruders.push(i);
        let lines = lines_for(&ev(TimelineKind::IntruderIdentified, "subject-1 identified: x"), &s);
        assert_eq!(lines[0].text, "Intruder identified. Caucasian male, forties, navy t-shirt, red beard.");
        assert!(!lines[0].text.contains("forensic paragraph"));
    }

    #[test]
    fn weapon_detected_names_intruder_and_weapon() {
        let mut s = case();
        s.intruders
            .push(intruder("subject-1", "tall, dark hoodie", true, Some("kitchen knife")));
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
}
