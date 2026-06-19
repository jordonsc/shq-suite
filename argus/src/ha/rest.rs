//! HA REST client — pulls a JPEG camera still in-memory via `/api/camera_proxy`.

use anyhow::{anyhow, Context, Result};

/// Thin REST client over the HA HTTP API.
pub struct RestClient {
    http: reqwest::Client,
    base_url: String,
    token: String,
}

impl RestClient {
    pub fn new(base_url: &str, token: &str) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token: token.to_string(),
        })
    }

    /// Fetch a current JPEG still for `entity` (a `camera.*` entity id).
    ///
    /// Uses `GET /api/camera_proxy/<entity>` which returns the image bytes
    /// in the response body (no temp file, unlike `camera/snapshot`).
    pub async fn snapshot(&self, entity: &str) -> Result<Vec<u8>> {
        let url = format!("{}/api/camera_proxy/{}", self.base_url, entity);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("camera_proxy request for {entity}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "camera_proxy for {entity} returned {status}: {body}"
            ));
        }

        let bytes = resp
            .bytes()
            .await
            .with_context(|| format!("reading camera bytes for {entity}"))?;
        Ok(bytes.to_vec())
    }

    /// Fetch the current `state` string of one entity via `GET /api/states/<id>`.
    /// Returns `None` if the entity is missing/unavailable (tolerated, not fatal).
    pub async fn state(&self, entity: &str) -> Result<Option<String>> {
        let url = format!("{}/api/states/{}", self.base_url, entity);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .with_context(|| format!("states request for {entity}"))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("states for {entity} returned {status}: {body}"));
        }

        let v: serde_json::Value = resp
            .json()
            .await
            .with_context(|| format!("parsing state for {entity}"))?;
        Ok(v.get("state").and_then(|s| s.as_str()).map(str::to_string))
    }

    /// Decide whether an activity `binary_sensor` is currently signalling.
    ///
    /// Returns `true` if `state == "on"`, OR if `state == "off"` AND its
    /// `last_changed` (RFC3339) is within `window_secs` of now — so a sensor that
    /// just dropped still counts as recent activity (debounce). EVERY other case —
    /// an `unavailable`/`unknown` state, a missing entity, a parse failure, or a
    /// transport error — returns `false` (treated as "no signal", never panics).
    /// This means an `unavailable` `_motion` sensor is simply ignored and its
    /// sibling `_person_detected` carries the gate.
    pub async fn sensor_active(&self, entity: &str, window_secs: u64) -> bool {
        let url = format!("{}/api/states/{}", self.base_url, entity);
        let resp = match self.http.get(&url).bearer_auth(&self.token).send().await {
            Ok(r) if r.status().is_success() => r,
            _ => return false,
        };
        let v: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(_) => return false,
        };
        match v.get("state").and_then(|s| s.as_str()) {
            Some("on") => true,
            Some("off") => {
                let Some(last) = v.get("last_changed").and_then(|s| s.as_str()) else {
                    return false;
                };
                let Ok(ts) = chrono::DateTime::parse_from_rfc3339(last) else {
                    return false;
                };
                let age = chrono::Utc::now().signed_duration_since(ts.with_timezone(&chrono::Utc));
                age.num_seconds() >= 0 && (age.num_seconds() as u64) <= window_secs
            }
            _ => false,
        }
    }

    /// Call an HA service: `POST /api/services/<domain>/<service>` with `data`
    /// as the JSON body. Used by the Phase 4 kiosk takeover to drive
    /// `shq_display.navigate`. Errors (non-2xx, transport) are returned, not
    /// fatal — the caller logs and continues (a takeover failure must never
    /// crash the engine).
    pub async fn call_service(
        &self,
        domain: &str,
        service: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        let url = format!("{}/api/services/{}/{}", self.base_url, domain, service);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(&data)
            .send()
            .await
            .with_context(|| format!("service call {domain}.{service}"))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "service {domain}.{service} returned {status}: {body}"
            ));
        }
        Ok(())
    }

    /// Pull a security-telemetry bundle for the configured entities, rendered as
    /// a compact text block to append after the cached seed. Unavailable
    /// entities are tolerated (skipped with their last-known/`unknown` state).
    /// Returns an empty string when no entities are configured.
    pub async fn telemetry(&self, entities: &[String]) -> String {
        if entities.is_empty() {
            return String::new();
        }
        let mut lines = Vec::with_capacity(entities.len());
        for entity in entities {
            let state = match self.state(entity).await {
                Ok(Some(s)) => s,
                Ok(None) => "unavailable".to_string(),
                Err(_) => "unknown".to_string(),
            };
            lines.push(format!("- {entity}: {state}"));
        }
        format!("Sensor telemetry (now):\n{}", lines.join("\n"))
    }
}
