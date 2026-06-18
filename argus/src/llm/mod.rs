//! Anthropic client — raw HTTP against `/v1/messages` (no official Rust SDK).
//!
//! Phase 1 makes one `claude-sonnet-4-6` vision call per still: the premises
//! seed as a `cache_control`-cached `system` block plus one base64 JPEG and a
//! short instruction. The tiered Sonnet/Opus loop and structured `CaseState`
//! arrive in Phase 2 — this client returns plain text.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::json;
use tracing::warn;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const MAX_TOKENS: u32 = 1024;

/// A completed assessment plus the usage block (so we can confirm the seed
/// cache is working: `cache_creation_input_tokens` on the first call,
/// `cache_read_input_tokens` on subsequent calls within the 5-min TTL).
#[derive(Debug, Clone)]
pub struct Assessment {
    pub text: String,
    pub usage: Usage,
}

/// Anthropic vision client, pinned to one model.
pub struct AnthropicClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .build()
            .context("building HTTP client")?;
        Ok(Self {
            http,
            api_key,
            model,
        })
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Assess one camera still. `seed` is the cached premises context; `jpeg` is
    /// the raw image bytes from HA `camera_proxy`.
    pub async fn assess(&self, seed: &str, camera_label: &str, jpeg: &[u8]) -> Result<Assessment> {
        let b64 = STANDARD.encode(jpeg);
        let prompt = format!(
            "Camera: {camera_label}. The intruder alarm has triggered. Describe what you see — \
             who is present, what they are doing, and where. If no person is visible, say so plainly."
        );

        // Stable content first (model, then the cached seed) so the cache prefix
        // holds; the volatile image + prompt come after the cache_control block.
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": [{
                "type": "text",
                "text": seed,
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [{
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": "image/jpeg",
                            "data": b64
                        }
                    },
                    { "type": "text", "text": prompt }
                ]
            }]
        });

        let resp = self
            .http
            .post(API_URL)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("anthropic request")?;

        let status = resp.status();
        let text = resp.text().await.context("reading anthropic response")?;
        if !status.is_success() {
            return Err(anyhow!("anthropic API returned {status}: {text}"));
        }

        let parsed: MessagesResponse = serde_json::from_str(&text)
            .with_context(|| format!("parsing anthropic response: {text}"))?;

        if parsed.stop_reason.as_deref() == Some("refusal") {
            warn!("anthropic returned a refusal stop_reason; assessment may be empty");
        }

        let assessment = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(Assessment {
            text: assessment,
            usage: parsed.usage,
        })
    }
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    usage: Usage,
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

/// Token-usage block from the Anthropic response.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
}
