//! Anthropic client — raw HTTP against `/v1/messages` (no official Rust SDK).
//!
//! Generalised for Phase 2: one client per model (`claude-sonnet-4-6` for the
//! live what/where loop, `claude-opus-4-8` for forensic identification), with
//! optional structured output (`output_config.format`) and optional adaptive
//! thinking + effort. The premises seed is a `cache_control`-cached `system`
//! block on every call (caches are model-scoped, 5-min TTL).
//!
//! Two reasoning profiles:
//! - [`Reasoning::Fast`] — thinking disabled, `effort: low`. The Sonnet live
//!   loop: cheap and quick, runs every tick.
//! - [`Reasoning::Deep`] — adaptive thinking, `effort: high`. The Opus forensic
//!   pass: lower-frequency, runs only when an intruder is new or improves.
//!
//! **No** `temperature`/`top_p`/`budget_tokens` — these 400 on 4.8.

use anyhow::{anyhow, Context, Result};
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::warn;

const API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// One camera still to include in a request.
pub struct ImageInput<'a> {
    pub label: &'a str,
    pub jpeg: &'a [u8],
}

/// Reasoning profile for a call (see module docs).
#[derive(Debug, Clone, Copy)]
pub enum Reasoning {
    /// Sonnet live loop: thinking disabled, `effort: low`.
    Fast,
    /// Opus forensic pass: adaptive thinking, `effort: high`.
    Deep,
}

/// Parameters for one assessment call.
pub struct AssessRequest<'a> {
    /// Cached premises seed (the `system` block).
    pub seed: &'a str,
    /// One or more camera stills (each preceded by a "Camera: <label>" text block).
    pub images: Vec<ImageInput<'a>>,
    /// The instruction text appended after the images.
    pub instruction: String,
    /// Optional JSON schema → `output_config.format` (structured output).
    pub schema: Option<Value>,
    pub max_tokens: u32,
    pub reasoning: Reasoning,
}

/// A completed call: the concatenated text (the JSON document when a schema was
/// supplied) plus the usage block.
#[derive(Debug, Clone)]
pub struct Completion {
    pub text: String,
    pub usage: Usage,
}

impl Completion {
    /// Parse the response text as JSON into `T` (use with a `schema`).
    pub fn parse<T: serde::de::DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_str(&self.text)
            .with_context(|| format!("parsing structured output: {}", self.text))
    }
}

/// Anthropic client pinned to one model. `Clone` is cheap (the `reqwest::Client`
/// is an `Arc` internally) so the forensic (Opus) client can be moved into a
/// spawned task to run concurrently with the live loop.
#[derive(Clone)]
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

    #[allow(dead_code)]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Run one `/v1/messages` call.
    pub async fn assess(&self, req: &AssessRequest<'_>) -> Result<Completion> {
        // Volatile content (images + instruction) goes AFTER the cached seed so
        // the cache prefix (model + seed) holds across ticks.
        let mut content: Vec<Value> = Vec::with_capacity(req.images.len() * 2 + 1);
        for img in &req.images {
            content.push(json!({ "type": "text", "text": format!("Camera: {}", img.label) }));
            content.push(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": "image/jpeg",
                    "data": STANDARD.encode(img.jpeg)
                }
            }));
        }
        content.push(json!({ "type": "text", "text": req.instruction }));

        let mut body = json!({
            "model": self.model,
            "max_tokens": req.max_tokens,
            "system": [{
                "type": "text",
                "text": req.seed,
                "cache_control": { "type": "ephemeral" }
            }],
            "messages": [{ "role": "user", "content": content }],
        });

        // output_config carries both effort and (optionally) the structured
        // output format.
        let mut output_config = serde_json::Map::new();
        match req.reasoning {
            Reasoning::Fast => {
                body["thinking"] = json!({ "type": "disabled" });
                output_config.insert("effort".into(), json!("low"));
            }
            Reasoning::Deep => {
                body["thinking"] = json!({ "type": "adaptive" });
                output_config.insert("effort".into(), json!("high"));
            }
        }
        if let Some(schema) = &req.schema {
            output_config.insert(
                "format".into(),
                json!({ "type": "json_schema", "schema": schema }),
            );
        }
        body["output_config"] = Value::Object(output_config);

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
            warn!("anthropic returned a refusal stop_reason; output may be empty");
        }

        // With output_config.format the first text block is the JSON document;
        // concatenating text blocks is harmless (thinking blocks are excluded).
        let out = parsed
            .content
            .iter()
            .filter(|b| b.kind == "text")
            .map(|b| b.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(Completion {
            text: out,
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

impl Usage {
    /// Accumulate another call's usage (for per-tick / daily cost tracking).
    #[allow(dead_code)]
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
    }

    /// Total billable input tokens (uncached + cache write + cache read).
    pub fn total_input(&self) -> u64 {
        self.input_tokens + self.cache_creation_input_tokens + self.cache_read_input_tokens
    }
}
