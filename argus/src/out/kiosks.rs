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
//! ## nyx DEPENDENCY (flagged — see 04-kiosk-hud.md Deviations)
//! A bare `shq_display.navigate` only issues a CDP `Page.navigate`. It does NOT
//! wake a sleeping kiosk nor kill a Chronos clock overlay (`idle_mode: clock`), so
//! on those kiosks the takeover would be invisible until nyx gains a wake/overlay-
//! kill on navigate. Kiosks with `idle_mode: off` are unaffected.

use tokio::sync::watch;
use tracing::{info, warn};

use crate::case::CaseState;
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
    // navigate only when the desired target actually changes.
    let mut current = Target::Dashboard;

    loop {
        // Recompute + navigate on a change to EITHER channel.
        let desired = {
            let case = case_rx.borrow();
            let mode = *mode_rx.borrow();
            desired_target(&case, mode)
        };
        if desired != current {
            match desired {
                Target::Hud => takeover(&rest, &kiosks, &alarm_url).await,
                Target::Dashboard => restore(&rest, &kiosks).await,
            }
            current = desired;
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

/// Navigate every kiosk to the alarm HUD.
async fn takeover(rest: &RestClient, kiosks: &[KioskConfig], alarm_url: &str) {
    info!(url = %alarm_url, "kiosks: TAKEOVER — navigating all kiosks to the HUD");
    for k in kiosks {
        navigate(rest, &k.ha_target, alarm_url).await;
    }
}

/// Navigate every kiosk back to its own dashboard.
async fn restore(rest: &RestClient, kiosks: &[KioskConfig]) {
    info!("kiosks: RESTORE — returning all kiosks to their dashboards");
    for k in kiosks {
        navigate(rest, &k.ha_target, &k.dashboard_url).await;
    }
}

/// Issue one `shq_display.navigate` call. The service takes `{device_id, url}`
/// (see home-assistant/shq_display/services.yaml). Logged-and-continue on any
/// failure — one unreachable kiosk must not stop the others.
async fn navigate(rest: &RestClient, device_id: &str, url: &str) {
    let data = serde_json::json!({ "device_id": device_id, "url": url });
    match rest.call_service("shq_display", "navigate", data).await {
        Ok(()) => info!(device_id, url, "kiosks: navigate ok"),
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
        // A gated (Investigate/General) case must NOT flip the kiosk while it
        // assesses silently — it is treated as "no case" with no active mode.
        assert!(
            desired_target(&case(TriggerProfile::Investigate), AlarmMode::Disarmed)
                == Target::Dashboard
        );
        assert!(
            desired_target(&case(TriggerProfile::General), AlarmMode::Disarmed)
                == Target::Dashboard
        );
    }

    #[test]
    fn mode_driven_panes_are_independent_of_a_gated_case() {
        // The standby/alarm panes ride the alarm MODE, not the case — a gated case
        // present during a real arm/trigger still shows the HUD via the mode.
        assert!(
            desired_target(&case(TriggerProfile::Investigate), AlarmMode::Armed) == Target::Hud
        );
        assert!(
            desired_target(&case(TriggerProfile::General), AlarmMode::Triggered) == Target::Hud
        );
    }
}
