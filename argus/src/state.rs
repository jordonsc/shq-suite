//! Alarm-mode tracking for the HA WS edge detection.
//!
//! Argus distinguishes the coarse alarm modes so it can both run the case state
//! machine (open on `triggered`, stand down on leaving it) AND drive the kiosk
//! HUD for the *non*-incident modes (arming / armed show the standby pane; only
//! the change-edges are surfaced so attribute-only `state_changed` repeats are
//! ignored). The rich case machine (`Triggered → Assessing → Standdown →
//! Cleared`) lives on `CaseState` in [`crate::case`].

/// The coarse alarm modes Argus distinguishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmMode {
    /// Disarmed / off (or unknown/unavailable — anything that isn't armed).
    Disarmed,
    /// Counting down to armed (the exit delay).
    Arming,
    /// Armed (any `armed_*`), incl. `pending` (the entry delay before a trigger).
    Armed,
    /// `triggered` — an incident; the engine opens a case.
    Triggered,
    /// Transient UI-only state: the green AUTHORISED "all clear" shown for a few
    /// seconds after a disarm that had no incident (never produced by `from_ha`;
    /// broadcast by the engine, then it reverts to `Disarmed`).
    Authorised,
}

impl AlarmMode {
    /// Map a raw `alarm_control_panel` state string to our coarse mode.
    pub fn from_ha(state: &str) -> Self {
        match state {
            "triggered" => AlarmMode::Triggered,
            "arming" => AlarmMode::Arming,
            // entry delay — still armed; the case only opens on `triggered`.
            "pending" => AlarmMode::Armed,
            "armed_away" | "armed_home" | "armed_night" | "armed_custom_bypass"
            | "armed_vacation" => AlarmMode::Armed,
            // disarmed / unknown / unavailable / anything else
            _ => AlarmMode::Disarmed,
        }
    }

    /// A human description of the arm state for the live-assessment prompt
    /// context (a prior that informs suspicion, e.g. an approach while
    /// `armed_away` weights higher — NOT a trigger).
    pub fn describe(self) -> &'static str {
        match self {
            AlarmMode::Disarmed => "disarmed (residents likely home)",
            AlarmMode::Arming => "arming (exit delay; residents leaving)",
            AlarmMode::Armed => "armed (residents likely out / asleep)",
            AlarmMode::Triggered => "triggered (alarm sounding)",
            AlarmMode::Authorised => "just disarmed (authorised all-clear)",
        }
    }

    /// The wire string sent to the HUD (`system` frame `mode`).
    pub fn as_str(self) -> &'static str {
        match self {
            AlarmMode::Disarmed => "disarmed",
            AlarmMode::Arming => "arming",
            AlarmMode::Armed => "armed",
            AlarmMode::Triggered => "triggered",
            AlarmMode::Authorised => "authorised",
        }
    }
}

/// Tracks the last-seen mode so only **edges** fire (not attribute-only repeats).
/// The first observation of any mode fires (so a startup query / first event
/// surfaces the current mode); thereafter only changes fire.
#[derive(Debug, Default)]
pub struct AlarmTracker {
    last: Option<AlarmMode>,
}

impl AlarmTracker {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Record a newly-observed mode. Returns `Some(mode)` on a change (incl. the
    /// first observation), `None` for a same-mode repeat.
    pub fn update(&mut self, new: AlarmMode) -> Option<AlarmMode> {
        if self.last == Some(new) {
            return None;
        }
        self.last = Some(new);
        Some(new)
    }
}
