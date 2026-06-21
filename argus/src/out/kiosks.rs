//! Phase 4 — kiosk takeover orchestration.
//!
//! A consumer that flips every configured wall kiosk to the Argus HUD whenever
//! the alarm warrants it, and back to its dashboard otherwise. It renders the
//! `(case, alarm-mode)` pair into `shq_display.navigate` HA service calls — it
//! does NOT call the LLM, and it does not touch the HUD's WS push (that's the web
//! server's job; this only points Chrome at `/alarm` or the dashboard).
//!
//! Desired kiosk target:
//! - **HUD** (`<public_base>/alarm`) when a case is present (triggered → assessing
//!   → cleared/AUTHORISED, held by the engine's authorised-dwell) OR the alarm is
//!   `arming`/`armed` (the standby pane). The HUD CONTENT (standby vs alarm vs
//!   green) is driven entirely by the `/kiosk` WS — the URL is the same `/alarm`
//!   for all of them, so arming→triggered→cleared needs NO re-navigation.
//! - **Dashboard** otherwise (disarmed, no case). The engine drops the cleared
//!   case off the broadcast after the dwell, which lands us here → restore.
//!
//! ## nyx wake/keep-awake (Phase 5 — resolved)
//! Every alarm-mode takeover navigate carries `wake: true` so nyx wakes the
//! backlight + kills any Chronos clock overlay (`idle_mode: clock`) BEFORE the
//! CDP navigate — the pane is visible even on a sleeping/clock kiosk, so the
//! `Triggered` takeover WAKES a kiosk that blanked while armed. During an actual
//! incident (`Triggered` or a live case) it also carries `keep_awake: true` to PIN
//! the screen on so the alarm HUD can't blank mid-incident; arming/armed/authorised
//! standby and the return to the dashboard send `keep_awake: false` so the kiosk
//! blanks normally while merely armed (no 24/7 burn when armed-away). Requires nyx >= 1.2.0 +
//! shq_display component >= 1.2.0; older nyx ignores the unknown fields (the
//! pre-Phase-5 navigate-only behaviour). See ledger shq-suite-0002.

use tokio::sync::watch;
use tracing::{info, warn};

use crate::case::{CaseState, CaseStatus};
use crate::config::KioskConfig;
use crate::ha::RestClient;
use crate::state::AlarmMode;

/// Where the kiosks should currently point.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    /// The Argus HUD (`/alarm`).
    Hud,
    /// Each kiosk's own dashboard.
    Dashboard,
}

/// The HUD is shown whenever there's a NON-GATED case (incl. the cleared/AUTHORISED
/// dwell) or the alarm is arming/armed; otherwise the dashboard.
///
/// **Gate (Phase 4a, dormant):** a GATED case (a softer `Investigate`/`General`
/// posture) must NOT take over the kiosk — it is treated as "no case" here, so the
/// kiosk stays on its dashboard while Argus assesses silently. The mode-driven
/// standby panes (arming/armed/triggered/authorised) are independent of any case
/// and are unaffected — a real alarm still flips the kiosk via the mode regardless.
/// No gated case opens today, so this is inert in 4a.
fn desired_target(case: &Option<CaseState>, mode: AlarmMode) -> Target {
    let active_case = case.as_ref().is_some_and(|c| !c.gated());
    if active_case
        || matches!(
            mode,
            AlarmMode::Arming | AlarmMode::Armed | AlarmMode::Triggered | AlarmMode::Authorised
        )
    {
        Target::Hud
    } else {
        Target::Dashboard
    }
}

/// Whether the kiosk should be PINNED awake (no idle blank/clock) — drives
/// `keep_awake`. The user's state machine:
/// - **Arming** → pinned (wake + keep on: the exit/entry countdown is an urgent
///   call-to-action, not a passive standby).
/// - **Armed** → NOT pinned (blanks normally; no 24/7 burn when armed-away).
/// - **Triggered** / a live (non-gated) case → pinned (the alarm HUD can't blank).
/// - **Standdown** (the post-disarm cleared-case dwell / `Authorised` green) → NOT
///   pinned: blanking is allowed, but with the idle timer reset to *now* (the pin
///   flipping off here makes the consumer re-navigate with `wake:true`, which resets
///   nyx's idle timer — so the blank countdown begins at standdown, not earlier).
///
/// The cleared case is EXCLUDED from `active_case` precisely so the pin drops at
/// standdown (a live case keeps it; the cleared dwell releases it).
fn alarm_active(case: &Option<CaseState>, mode: AlarmMode) -> bool {
    let live_case = case
        .as_ref()
        .is_some_and(|c| !c.gated() && c.status != CaseStatus::Cleared);
    live_case || matches!(mode, AlarmMode::Triggered | AlarmMode::Arming)
}

/// Spawned in daemon mode only (and only when `web` is configured + `kiosks` is
/// non-empty). Owns its own `watch` receivers + a `RestClient` for the service
/// calls. Failures are logged, never fatal.
pub async fn run(
    mut case_rx: watch::Receiver<Option<CaseState>>,
    mut mode_rx: watch::Receiver<AlarmMode>,
    kiosks: Vec<KioskConfig>,
    rest: RestClient,
    public_base: String,
) {
    let alarm_url = format!("{}/alarm", public_base.trim_end_matches('/'));
    info!(
        kiosks = kiosks.len(),
        alarm_url = %alarm_url,
        "kiosks: Phase 4 takeover consumer started"
    );

    // What the kiosks currently show. Assume they boot on their dashboards; we
    // navigate only when the desired target — or the keep-awake pin — changes.
    let mut current = Target::Dashboard;
    let mut current_force_on = false;
    let mut prev_mode = AlarmMode::Disarmed;

    loop {
        // Recompute + navigate on a change to EITHER channel.
        let (desired, force_on, mode) = {
            let case = case_rx.borrow();
            let mode = *mode_rx.borrow();
            (desired_target(&case, mode), alarm_active(&case, mode), mode)
        };
        // Re-navigate on a target change OR when the keep-awake pin flips while
        // staying on the HUD. arming→triggered keeps the SAME `/alarm` URL (the
        // target doesn't change), so without the pin check the `keep_awake:true`
        // pin computed for the live alarm would NEVER be sent — the screens get
        // woken at arm-time but could blank during the actual alarm.
        let target_changed = desired != current;
        let pin_changed = desired == Target::Hud && force_on != current_force_on;
        // Entering the NO-INCIDENT all-clear (`Authorised` green, no case): target
        // and pin may both be unchanged (armed→authorised are both unpinned HUD), so
        // force a re-navigate to WAKE the kiosk + reset the idle timer (the takeover's
        // `wake:true`) so the green is shown and the blank countdown restarts at
        // standdown. (The post-incident standdown is handled by `pin_changed` — the
        // live→cleared pin drop re-navigates with the same wake.)
        let entered_authorised = desired == Target::Hud
            && mode == AlarmMode::Authorised
            && prev_mode != AlarmMode::Authorised;
        prev_mode = mode;
        if target_changed || pin_changed || entered_authorised {
            match desired {
                // Wake the screen on every alarm-mode takeover so the standby/
                // alarm pane is visible even on a sleeping/clock kiosk; PIN it
                // awake (`keep_awake`) only while an alarm is actually ACTIVE
                // (Triggered or a live case) so arming/armed/authorised standby
                // can still blank normally.
                Target::Hud => takeover(&rest, &kiosks, &alarm_url, force_on).await,
                // Returning to the dashboard always releases the keep-awake pin so
                // normal idle/blank resumes.
                Target::Dashboard => restore(&rest, &kiosks).await,
            }
            current = desired;
            current_force_on = force_on;
        }

        tokio::select! {
            changed = case_rx.changed() => if changed.is_err() {
                warn!("kiosks: case broadcast closed; exiting");
                break;
            },
            changed = mode_rx.changed() => if changed.is_err() {
                warn!("kiosks: mode broadcast closed; exiting");
                break;
            },
        }
    }
}

/// Navigate every kiosk to the alarm HUD. Always `wake: true` (the takeover must
/// be visible on a sleeping/clock kiosk); `keep_awake` = `force_on` (pin the
/// screen on only while the alarm is actively sounding/assessing).
async fn takeover(rest: &RestClient, kiosks: &[KioskConfig], alarm_url: &str, force_on: bool) {
    info!(url = %alarm_url, keep_awake = force_on, "kiosks: TAKEOVER — navigating all kiosks to the HUD");
    for k in kiosks {
        navigate(rest, &k.ha_target, alarm_url, true, force_on).await;
    }
}

/// Navigate every kiosk back to its own dashboard. Releases the keep-awake pin
/// (`keep_awake: false`) so normal idle/blank resumes; no need to force a wake.
async fn restore(rest: &RestClient, kiosks: &[KioskConfig]) {
    info!("kiosks: RESTORE — returning all kiosks to their dashboards");
    for k in kiosks {
        navigate(rest, &k.ha_target, &k.dashboard_url, false, false).await;
    }
}

/// Issue one `shq_display.navigate` call. The service takes
/// `{device_id, url, wake, keep_awake}` (see home-assistant/shq_display/
/// services.yaml) — `wake`/`keep_awake` drive nyx's screen wake + keep-awake pin
/// (nyx >= 1.2.0; older nyx ignores them). Logged-and-continue on any failure —
/// one unreachable kiosk must not stop the others.
async fn navigate(rest: &RestClient, device_id: &str, url: &str, wake: bool, keep_awake: bool) {
    let data = serde_json::json!({
        "device_id": device_id,
        "url": url,
        "wake": wake,
        "keep_awake": keep_awake,
    });
    match rest.call_service("shq_display", "navigate", data).await {
        Ok(()) => info!(device_id, url, wake, keep_awake, "kiosks: navigate ok"),
        Err(e) => warn!(device_id, url, error = %e, "kiosks: navigate failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::TriggerProfile;
    use chrono::Utc;

    fn case(profile: TriggerProfile) -> Option<CaseState> {
        Some(CaseState::new("case-test".to_string(), Utc::now(), profile))
    }

    #[test]
    fn alarm_case_takes_over_kiosk() {
        // Today's behaviour: an Alarm case (disarmed mode in --once, or triggered)
        // drives the HUD takeover. Unchanged by 4a.
        assert!(desired_target(&case(TriggerProfile::Alarm), AlarmMode::Disarmed) == Target::Hud);
        assert!(desired_target(&None, AlarmMode::Triggered) == Target::Hud);
    }

    #[test]
    fn gated_case_does_not_take_over() {
        // A gated (Investigate) case must NOT flip the kiosk while it assesses
        // silently — it is treated as "no case" with no active mode.
        assert!(
            desired_target(&case(TriggerProfile::Investigate), AlarmMode::Disarmed)
                == Target::Dashboard
        );
    }

    #[test]
    fn alarm_active_pins_for_arming_triggered_and_live_case() {
        // Pinned: the Arming delay, a Triggered alarm, and a live (non-gated) case.
        assert!(alarm_active(&None, AlarmMode::Arming));
        assert!(alarm_active(&None, AlarmMode::Triggered));
        assert!(alarm_active(&case(TriggerProfile::Alarm), AlarmMode::Disarmed));
        // NOT pinned: steady armed / disarmed standby (the kiosk blanks normally).
        assert!(!alarm_active(&None, AlarmMode::Armed));
        assert!(!alarm_active(&None, AlarmMode::Disarmed));
        // NOT pinned: the STANDDOWN dwell — a CLEARED case (the pin must drop here so
        // the consumer re-navigates with wake:true → idle timer resets to now)...
        let mut cleared = CaseState::new("c".to_string(), Utc::now(), TriggerProfile::Alarm);
        cleared.status = CaseStatus::Cleared;
        assert!(!alarm_active(&Some(cleared), AlarmMode::Disarmed));
        // ...and the no-incident Authorised green is likewise unpinned (blank allowed).
        assert!(!alarm_active(&None, AlarmMode::Authorised));
        // NOT pinned: a gated (soft) case that hasn't escalated.
        assert!(!alarm_active(&case(TriggerProfile::Investigate), AlarmMode::Armed));
    }

    #[test]
    fn mode_driven_panes_are_independent_of_a_gated_case() {
        // The standby/alarm panes ride the alarm MODE, not the case — a gated case
        // present during a real arm/trigger still shows the HUD via the mode.
        assert!(
            desired_target(&case(TriggerProfile::Investigate), AlarmMode::Armed) == Target::Hud
        );
        assert!(
            desired_target(&case(TriggerProfile::Investigate), AlarmMode::Triggered) == Target::Hud
        );
    }
}
