//! Phase 4 — kiosk takeover orchestration.
//!
//! A `watch` consumer that flips every configured wall kiosk to the HUD on the
//! first `CaseState` of a case, and back to its dashboard when the case clears.
//! It renders `CaseState` lifecycle into `shq_display.navigate` HA service calls
//! — it does NOT call the LLM, and it does not touch the HUD's WS push (that's
//! the web server's job; this only points Chrome at `/alarm`).
//!
//! Lifecycle (per `case_id`, deduped so we navigate at most once each way):
//! - **Takeover** on the first state of a new case (status `triggered` /
//!   `assessing`): navigate every kiosk → `<public_base>/alarm`.
//! - **Restore** on `cleared`: navigate every kiosk → its `dashboard_url`.
//!   (`standdown` is the disarm edge but not terminal; we restore on the
//!   terminal `cleared` so the AUTHORISED/green state has been shown first.)
//!
//! ## nyx DEPENDENCY (flagged — see 04-kiosk-hud.md Deviations)
//! A bare `shq_display.navigate` only issues a CDP `Page.navigate`. It does NOT
//! wake a sleeping kiosk (backlight off) and does NOT kill a Chronos clock
//! overlay (which sits *above* Chrome on `idle_mode: clock` kiosks) — so on
//! those kiosks the takeover would be invisible. nyx's `wake`/`set_display true`
//! is what SIGKILLs Chronos + restores the backlight (see `nyx/CLAUDE.md` and
//! ledger `shq-suite-0001`). The clean fix is a small nyx change so `navigate`
//! IMPLIES wake+overlay-kill; until then a `wake` must precede the `navigate`.
//! That nyx change / the wake call is NOT yet wired here (no `shq_display.wake`
//! service exists today) — documented as the remaining work.

use tokio::sync::watch;
use tracing::{info, warn};

use crate::case::{CaseState, CaseStatus};
use crate::config::KioskConfig;
use crate::ha::RestClient;

/// What we last did for the current case, so we navigate at most once each way.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Kiosks flipped to the HUD.
    TakenOver,
    /// Kiosks returned to their dashboards.
    Restored,
}

/// Spawned in daemon mode only (and only when `web` is configured + `kiosks` is
/// non-empty). Owns its own `watch` receiver + a `RestClient` for the service
/// calls. Failures are logged, never fatal.
pub async fn run(
    mut rx: watch::Receiver<Option<CaseState>>,
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

    // Per-case dedup: the case id we are currently driving + which way we last
    // navigated. A new case id resets the phase.
    let mut active_case: Option<String> = None;
    let mut phase: Option<Phase> = None;

    loop {
        if rx.changed().await.is_err() {
            warn!("kiosks: broadcast closed; exiting");
            break;
        }
        let state = match rx.borrow_and_update().clone() {
            Some(s) => s,
            None => continue,
        };

        // New case → reset the dedup so the next branch can take over.
        if active_case.as_deref() != Some(state.case_id.as_str()) {
            active_case = Some(state.case_id.clone());
            phase = None;
        }

        match state.status {
            CaseStatus::Triggered | CaseStatus::Assessing => {
                if phase != Some(Phase::TakenOver) {
                    takeover(&rest, &kiosks, &alarm_url).await;
                    phase = Some(Phase::TakenOver);
                }
            }
            CaseStatus::Cleared => {
                if phase != Some(Phase::Restored) {
                    restore(&rest, &kiosks).await;
                    phase = Some(Phase::Restored);
                }
            }
            // Standdown is the disarm edge but not terminal — hold the HUD up
            // (the AUTHORISED state renders here) until `cleared` restores.
            CaseStatus::Standdown => {}
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
