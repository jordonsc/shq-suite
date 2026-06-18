//! Coarse alarm-state tracking for the HA WS edge detection.
//!
//! Argus only needs the **edges** of the HA alarm: into `triggered` (open a
//! case) and out of `triggered` (stand the case down). The rich case state
//! machine (`Triggered → Assessing → Standdown → Cleared`) lives on `CaseState`
//! in [`crate::case`]; this type just turns a stream of HA `state_changed`
//! events into those two transitions.

/// The alarm states Argus distinguishes for edge detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlarmState {
    /// Any non-triggered HA alarm state (disarmed, armed_*, pending, arming, …).
    Disarmed,
    /// The HA alarm is in `triggered`.
    Triggered,
}

impl AlarmState {
    /// Map a raw `alarm_control_panel` state string to our coarse enum.
    pub fn from_ha(state: &str) -> Self {
        if state == "triggered" {
            AlarmState::Triggered
        } else {
            AlarmState::Disarmed
        }
    }
}

/// An edge worth acting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Entered `triggered` — open a case.
    IntoTriggered,
    /// Left `triggered` — stand the case down.
    OutOfTriggered,
}

/// Tracks the last-seen alarm state so only **edges** fire (not repeats, e.g. a
/// triggered→triggered attribute-only `state_changed`).
#[derive(Debug, Default)]
pub struct AlarmTracker {
    last: Option<AlarmState>,
}

impl AlarmTracker {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Record a newly-observed state. Returns `Some(transition)` only on an edge
    /// into or out of `Triggered`; `None` for same-state repeats. The very first
    /// observation of `Triggered` fires `IntoTriggered`; the first observation
    /// of a non-triggered state does **not** fire (we never saw a trigger to
    /// stand down).
    pub fn update(&mut self, new: AlarmState) -> Option<Transition> {
        let transition = match (self.last, new) {
            (Some(AlarmState::Triggered), AlarmState::Triggered) => None,
            (_, AlarmState::Triggered) => Some(Transition::IntoTriggered),
            (Some(AlarmState::Triggered), AlarmState::Disarmed) => {
                Some(Transition::OutOfTriggered)
            }
            (_, AlarmState::Disarmed) => None,
        };
        self.last = Some(new);
        transition
    }
}
