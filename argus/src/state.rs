//! Minimal alarm-state machine for Phase 1.
//!
//! Phase 1 only needs to detect the transition **into** `triggered`. Everything
//! that is not `triggered` collapses to `Disarmed` here; the full machine
//! (armed_home/away, pending, arming, …) and the structured `CaseState` arrive
//! in Phase 2.

/// The alarm states Argus distinguishes in Phase 1.
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

/// Tracks the last-seen alarm state so only the **edge** into `Triggered` fires,
/// not repeats (e.g. a triggered→triggered attribute-only `state_changed`).
#[derive(Debug, Default)]
pub struct AlarmTracker {
    last: Option<AlarmState>,
}

impl AlarmTracker {
    pub fn new() -> Self {
        Self { last: None }
    }

    /// Record a new observed state. Returns `true` iff this is a transition
    /// into `Triggered` (i.e. the previous state was not already `Triggered`).
    pub fn update(&mut self, new: AlarmState) -> bool {
        let fired = new == AlarmState::Triggered && self.last != Some(AlarmState::Triggered);
        self.last = Some(new);
        fired
    }
}
