//! Phase 3 — PagerDuty Events v2 security-station dispatch.
//!
//! Posts a dossier to PagerDuty (`POST https://events.pagerduty.com/v2/enqueue`)
//! keyed on `case_id` (the dedup key):
//! - **`trigger`** on case open, and re-sent (same dedup_key) on a *material
//!   change* (new intruder, `intruder_identified`, threat escalation) so the
//!   open incident always reflects the latest dossier.
//! - **`resolve`** (same dedup_key) on standdown / cleared.
//!
//! `payload.severity` derives from [`ThreatLevel::pagerduty_severity`];
//! `payload.summary` is `CaseState.summary`; `custom_details` carries the
//! threat level, intruders (descriptors/confidence/dossier), the recent timeline,
//! and the deterministic S3 case prefix `s3://<bucket>/<prefix>/<case_id>/` when
//! offsite is configured (per the 02a "Inputs to Phase 3" — atlas holds
//! write-only creds, so we embed the BARE prefix, never a presigned URL).
//!
//! The body builder ([`build_event`]) is split from the sender ([`PagerDuty::send`])
//! so the request **shape** can be unit/shape-tested without any network call —
//! which is all this phase does (live-fire is deferred).
//!
//! **Resilience contract:** `send` logs and returns on any error — never panics,
//! never blocks the voice channel. The routing key is NEVER logged.

use serde::Serialize;
use serde_json::{json, Value};
use tracing::{info, warn};

use crate::case::CaseState;
use crate::config::OffsiteConfig;

const ENQUEUE_URL: &str = "https://events.pagerduty.com/v2/enqueue";

/// PagerDuty Events v2 `event_action`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Trigger,
    Resolve,
}

impl Action {
    fn as_str(self) -> &'static str {
        match self {
            Action::Trigger => "trigger",
            Action::Resolve => "resolve",
        }
    }
}

/// The Events v2 enqueue body. Serialises to the exact JSON PagerDuty expects.
/// `routing_key` is `#[serde(skip)]`-free (the API requires it) but this struct
/// is only ever logged via its non-secret fields — see [`PagerDuty::send`].
#[derive(Debug, Clone, Serialize)]
pub struct EventBody {
    pub routing_key: String,
    pub event_action: &'static str,
    pub dedup_key: String,
    /// Only present on `trigger` (PagerDuty ignores `payload` on `resolve`, but
    /// omitting it keeps the resolve body minimal and unambiguous).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Payload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Payload {
    pub summary: String,
    pub source: String,
    pub severity: &'static str,
    pub custom_details: Value,
}

/// The PagerDuty sink. Holds the routing key + source; reused across a case.
#[derive(Clone)]
pub struct PagerDuty {
    routing_key: String,
    source: String,
    http: reqwest::Client,
}

impl PagerDuty {
    /// Construct from config. An empty `routing_key` means the caller should not
    /// have spawned this at all — but [`PagerDuty::send`] also guards against it
    /// defensively (logs + no-op) so a misconfiguration can never page blindly.
    pub fn new(routing_key: String, source: String) -> Self {
        Self {
            routing_key,
            source,
            http: reqwest::Client::new(),
        }
    }

    /// BUILD the Events v2 body for `state` + `action`. PURE — no network. This is
    /// the shape-test surface. `offsite`, when `enabled`, contributes the bare S3
    /// case prefix to `custom_details`.
    pub fn build_event(
        &self,
        state: &CaseState,
        action: Action,
        offsite: Option<&OffsiteConfig>,
    ) -> EventBody {
        let payload = match action {
            Action::Trigger => Some(Payload {
                summary: state.summary.clone(),
                source: self.source.clone(),
                severity: state.threat_level.pagerduty_severity(),
                custom_details: custom_details(state, offsite),
            }),
            // resolve: dedup_key alone closes the incident.
            Action::Resolve => None,
        };
        EventBody {
            routing_key: self.routing_key.clone(),
            event_action: action.as_str(),
            dedup_key: state.case_id.clone(),
            payload,
        }
    }

    /// SEND a pre-built body to PagerDuty. Logs + returns on any failure; never
    /// panics. Never logs the routing key.
    pub async fn send(&self, body: &EventBody) {
        if self.routing_key.trim().is_empty() {
            warn!("pagerduty: routing_key empty; skipping {} send", body.event_action);
            return;
        }
        match self.http.post(ENQUEUE_URL).json(body).send().await {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    info!(
                        action = body.event_action,
                        dedup_key = %body.dedup_key,
                        "pagerduty: {} accepted ({})",
                        body.event_action,
                        status
                    );
                } else {
                    let text = resp.text().await.unwrap_or_default();
                    warn!(
                        action = body.event_action,
                        dedup_key = %body.dedup_key,
                        "pagerduty: {} rejected: {} {}",
                        body.event_action,
                        status,
                        text
                    );
                }
            }
            Err(e) => warn!(
                action = body.event_action,
                dedup_key = %body.dedup_key,
                "pagerduty: {} send failed: {e}",
                body.event_action
            ),
        }
    }

    /// Convenience: build + send a `trigger` for the current state.
    pub async fn trigger(&self, state: &CaseState, offsite: Option<&OffsiteConfig>) {
        let body = self.build_event(state, Action::Trigger, offsite);
        self.send(&body).await;
    }

    /// Convenience: build + send a `resolve` for the case.
    pub async fn resolve(&self, state: &CaseState) {
        let body = self.build_event(state, Action::Resolve, None);
        self.send(&body).await;
    }
}

/// Build the `custom_details` dossier object for a trigger.
fn custom_details(state: &CaseState, offsite: Option<&OffsiteConfig>) -> Value {
    let intruders: Vec<Value> = state
        .intruders
        .iter()
        .map(|i| {
            json!({
                "id": i.id,
                "descriptors": i.descriptors,
                "confidence": i.confidence,
                "identified": i.identified,
                "armed": i.armed,
                "weapon": i.weapon,
                "location": i.location,
                "activity": i.activity,
                "dossier": i.dossier,
            })
        })
        .collect();

    // Most recent timeline events (closed-vocabulary milestones), newest last.
    let timeline: Vec<Value> = state
        .timeline
        .iter()
        .rev()
        .take(10)
        .rev()
        .map(|e| {
            json!({
                "at": e.at,
                "kind": e.kind,      // snake_case via serde on TimelineKind
                "detail": e.detail,
            })
        })
        .collect();

    let mut details = json!({
        "case_id": state.case_id,
        "threat_level": state.threat_level,   // snake_case via serde
        "status": state.status,
        "started_at": state.started_at,
        "updated_at": state.updated_at,
        "intruders": intruders,
        "threats": state.threats,
        "timeline": timeline,
    });

    // Embed the deterministic S3 case prefix IF offsite is configured. Per 02a:
    // atlas has write-only creds, so this is the BARE prefix, not a presigned URL.
    if let Some(o) = offsite {
        if o.enabled && !o.bucket.trim().is_empty() {
            let prefix = o.prefix.trim_matches('/');
            let s3 = format!("s3://{}/{}/{}/", o.bucket, prefix, state.case_id);
            details["evidence_s3_prefix"] = Value::String(s3);
        }
    }

    details
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::CaseState;
    use chrono::Utc;

    fn case() -> CaseState {
        let mut s = CaseState::new("case-20260618T120000Z".to_string(), Utc::now());
        s.summary = "Person at the garage side door, forcing entry.".to_string();
        s
    }

    #[test]
    fn trigger_body_shape() {
        let pd = PagerDuty::new("ROUTING_KEY_PLACEHOLDER".to_string(), "argus@atlas".to_string());
        let body = pd.build_event(&case(), Action::Trigger, None);
        assert_eq!(body.event_action, "trigger");
        assert_eq!(body.dedup_key, "case-20260618T120000Z");
        let p = body.payload.as_ref().expect("trigger has payload");
        assert_eq!(p.source, "argus@atlas");
        // Default ThreatLevel for a new case is Elevated → "warning".
        assert_eq!(p.severity, "warning");
        // Serialises cleanly to the Events v2 shape.
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(v["event_action"], "trigger");
        assert_eq!(v["dedup_key"], "case-20260618T120000Z");
        assert!(v["payload"]["custom_details"]["timeline"].is_array());
    }

    #[test]
    fn trigger_embeds_s3_prefix_when_offsite_enabled() {
        let pd = PagerDuty::new("ROUTING_KEY_PLACEHOLDER".to_string(), "argus@atlas".to_string());
        let mut off = OffsiteConfig::default();
        off.enabled = true;
        off.bucket = "shq-argus-cases".to_string();
        off.prefix = "cases".to_string();
        let body = pd.build_event(&case(), Action::Trigger, Some(&off));
        let v = serde_json::to_value(&body).unwrap();
        assert_eq!(
            v["payload"]["custom_details"]["evidence_s3_prefix"],
            "s3://shq-argus-cases/cases/case-20260618T120000Z/"
        );
    }

    #[test]
    fn resolve_body_has_no_payload() {
        let pd = PagerDuty::new("ROUTING_KEY_PLACEHOLDER".to_string(), "argus@atlas".to_string());
        let body = pd.build_event(&case(), Action::Resolve, None);
        assert_eq!(body.event_action, "resolve");
        assert!(body.payload.is_none());
        let v = serde_json::to_value(&body).unwrap();
        assert!(v.get("payload").is_none(), "resolve omits payload");
    }
}
