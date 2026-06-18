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
