//! Configuration loading for Argus.
//!
//! Config lives at `~/.config/argus/config.yaml` (override with `--config`).
//! Secrets are kept out of the file: any `${VAR}` in the YAML is expanded from
//! the environment at load time (systemd `EnvironmentFile`, or the shell).

use std::path::PathBuf;

use std::collections::HashMap;

use anyhow::{anyhow, Context, Result};
use directories::{BaseDirs, ProjectDirs};
use serde::Deserialize;

use crate::case::TriggerProfile;

/// Top-level Argus configuration (`config.yaml`).
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub ha: HaConfig,
    pub anthropic: AnthropicConfig,
    /// IANA timezone name (e.g. `Australia/Sydney`) used to localise the current
    /// date/time injected into the live-assessment prompt context. The container
    /// runs UTC, so this is parsed with `chrono_tz::Tz` and `Utc::now()` is
    /// converted into it — do NOT use `chrono::Local`. An invalid/unknown name
    /// falls back to UTC with a warning (never fatal). Default `"UTC"`.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Path to the private premises seed (env/`~` expanded).
    pub seed_path: String,
    /// Cameras Argus captures each assessment tick.
    pub cameras: Vec<CameraConfig>,
    /// The HA alarm entity Argus subscribes to. This is the **legacy single
    /// trigger** — still the only one the live atlas config sets, and still the
    /// `Alarm`-profile source. When `triggers` is empty it is synthesised into a
    /// single `Alarm` trigger (see [`Config::trigger_sources`]).
    pub alarm_entity: String,
    /// Per-source trigger profiles (Phase 4a). Each entry maps an HA entity to the
    /// [`TriggerProfile`] a case opened by it should take. EMPTY (the default, and
    /// the live atlas config today) ⇒ back-compat: a single `{ entity:
    /// alarm_entity, profile: Alarm }` is synthesised, so behaviour is identical to
    /// pre-4a. The softer (`investigate`/`general`) sources are wired at test time
    /// (Phase 4b); 4a only carries the config shape.
    #[serde(default)]
    pub triggers: Vec<TriggerSourceConfig>,
    /// Optional HA entity whose state is the human location that tripped the alarm
    /// (e.g. `input_text.alarm_trigger_room`, set by the existing HA "Alarm
    /// Sensors" automation to `area_name(trigger.entity_id)`). Read once at case
    /// open to drive the spoken breach line. Absent = the breach line is generic.
    #[serde(default)]
    pub trigger_location_entity: Option<String>,
    /// Optional HA entity that, on ANY state change, requests a panel-independent
    /// STANDDOWN of the active case (the kill switch). Typically an
    /// `input_button` (e.g. `input_button.argus_stand_down`) — pressing it changes
    /// its state to a fresh timestamp, so the WS treats *any* `state_changed` on it
    /// as a press and emits [`crate::ha::HaEvent::StandDownRequested`]. This is the
    /// off-switch for the klaxon/PD/kiosk when the manual alarm panel was DISARMED
    /// and so never entered `triggered` (no panel edge to disarm). Absent = no kill
    /// switch (the panel disarm + the Phase-5 control WS remain the only standdowns).
    #[serde(default)]
    pub standdown_entity: Option<String>,
    /// Optional HA script (name only, e.g. `argus_alarm` → `script.argus_alarm`)
    /// that trips the real alarm panel WITH the secret code, HA-side. When set, an
    /// escalation calls `script.<name>` instead of `alarm_control_panel.alarm_trigger`
    /// — the trip code then lives in HA, not in Argus's config/seed. Absent = fall
    /// back to the direct `alarm_trigger` on the primary alarm entity.
    #[serde(default)]
    pub escalation_alarm_script: Option<String>,
    /// The assessment-loop cadence + guardrails (Phase 2).
    #[serde(default)]
    pub loop_config: LoopConfig,
    /// Security-relevant HA entities pulled as per-tick telemetry (door/window/
    /// motion/reed/DOSA). Rendered as text after the cached seed.
    #[serde(default)]
    pub telemetry_entities: Vec<String>,
    /// Offsite case replication to S3 (Phase 2a). Absent block = disabled.
    #[serde(default)]
    pub offsite: OffsiteConfig,
    /// Overwatch gRPC voice/klaxon output (Phase 3). Absent = voice disabled.
    #[serde(default)]
    pub overwatch: Option<OverwatchConfig>,
    /// PagerDuty security-station output (Phase 3). Absent (or an empty
    /// `routing_key`) = PagerDuty disabled.
    #[serde(default)]
    pub pagerduty: Option<PagerDutyConfig>,
    /// Kiosk HUD HTTP/WS server (Phase 4). Absent = the HUD server is not
    /// spawned (no `/alarm`, `/stills`, `/kiosk`).
    #[serde(default)]
    pub web: Option<WebConfig>,
    /// Wall kiosks to take over on trigger (Phase 4). Empty = no takeover (the
    /// HUD is still served and reachable, just not pushed to any kiosk). Only
    /// driven when `web` is also configured (the takeover URL is `web.public_base`).
    #[serde(default)]
    pub kiosks: Vec<KioskConfig>,
}

/// One watched trigger source, tagged with the [`TriggerProfile`] a case opened
/// by it should take. The house alarm is `Alarm`; a front-door / presumed-benign
/// approach is `Investigate`. An omitted `profile` defaults to `Alarm`.
#[derive(Debug, Clone, Deserialize)]
pub struct TriggerSourceConfig {
    /// The HA entity whose transition opens a case.
    pub entity: String,
    /// The profile a case from this source takes. Default `Alarm`.
    #[serde(default)]
    pub profile: TriggerProfile,
}

/// The Phase 4 kiosk HUD server: serves the `argus/web/` app, the case stills,
/// and the `/kiosk` WebSocket that pushes `CaseState`. Absent = not spawned.
#[derive(Debug, Clone, Deserialize)]
pub struct WebConfig {
    /// Listen address. Bind on the LAN (atlas) so kiosks can reach it.
    #[serde(default = "default_web_bind")]
    pub bind: String,
    /// The base URL kiosks navigate to (`<public_base>/alarm`). This is what
    /// `shq_display.navigate` sends — it must be the LAN-reachable address of
    /// THIS server, not the bind (which may be `0.0.0.0`).
    #[serde(default = "default_web_public_base")]
    pub public_base: String,
    /// Directory of the static HUD app to serve (`index.html` etc.). Relative
    /// paths resolve against the argus binary's own directory (so a deployed
    /// `web/` sitting next to the binary just works); absolute paths are used
    /// verbatim. Default: `web`.
    #[serde(default = "default_web_dir")]
    pub dir: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            bind: default_web_bind(),
            public_base: default_web_public_base(),
            dir: default_web_dir(),
        }
    }
}

fn default_web_bind() -> String {
    "0.0.0.0:8770".to_string()
}

fn default_web_public_base() -> String {
    "http://atlas:8770".to_string()
}

fn default_web_dir() -> String {
    "web".to_string()
}

/// One wall kiosk to take over on trigger (Phase 4).
#[derive(Debug, Clone, Deserialize)]
pub struct KioskConfig {
    /// The `shq_display` device id (the `device_id` field of the
    /// `shq_display.navigate` service — the kiosk key from HA's `configuration.yaml`).
    pub ha_target: String,
    /// The URL to navigate this kiosk BACK to on disarm (its normal dashboard).
    pub dashboard_url: String,
}

/// Overwatch gRPC voice server (Phase 3 output sink).
///
/// Present = Argus speaks positive-only milestones and drives the klaxon via
/// Overwatch's `VoiceService` (gRPC, port 50051). Absent = the voice channel is
/// not spawned. Connection/RPC failures are logged, never fatal.
#[derive(Debug, Clone, Deserialize)]
pub struct OverwatchConfig {
    /// Overwatch host (the voice RPi). gRPC is plaintext on the LAN.
    pub host: String,
    /// gRPC port.
    #[serde(default = "default_overwatch_port")]
    pub port: u16,
    /// `SetAlarm` alarm_id (a key in Overwatch's `alarms` config map).
    #[serde(default = "default_alarm_id")]
    pub alarm_id: String,
    /// AWS Polly voice id for `Verbalise` (default "Amy").
    #[serde(default = "default_voice_id")]
    pub voice_id: String,
    /// Whether to sound the klaxon (`SetAlarm`) at all. `false` = voice-only
    /// (Argus still speaks, but never starts the siren) — useful for testing
    /// without distressing pets/neighbours. Default `true`.
    #[serde(default = "default_true")]
    pub klaxon_enabled: bool,
    /// Klaxon (`SetAlarm`) volume 0.0–1.0. The klaxon is LOUD — set low for
    /// testing. Real-incident value is ~0.9.
    #[serde(default = "default_klaxon_volume")]
    pub klaxon_volume: f32,
    /// Spoken-line (`Verbalise`) volume 0.0–1.0. Real-incident value is ~1.0.
    #[serde(default = "default_voice_volume")]
    pub voice_volume: f32,
    /// Factor to scale a sounding klaxon by while a line is spoken, so the speech
    /// stays intelligible over it (`Verbalise.duck_alarm_factor`), e.g. 0.5 = half.
    /// 0 = no ducking. Restored to `klaxon_volume` after the clip. Requires
    /// Overwatch ≥0.4.1 (an older server ignores the unknown field). Default 0.5.
    #[serde(default = "default_duck_factor")]
    pub duck_factor: f32,
}

fn default_overwatch_port() -> u16 {
    50051
}

fn default_alarm_id() -> String {
    "security".to_string()
}

fn default_voice_id() -> String {
    "Amy".to_string()
}

fn default_true() -> bool {
    true
}

fn default_klaxon_volume() -> f32 {
    0.9
}

fn default_voice_volume() -> f32 {
    1.0
}

fn default_duck_factor() -> f32 {
    0.5
}

/// PagerDuty Events v2 security-station dispatch (Phase 3 output sink).
///
/// The `routing_key` arrives via `${PAGERDUTY_ROUTING_KEY}` env-expansion and may
/// be **empty or absent** — that is NOT a startup blocker, it simply disables the
/// PagerDuty channel (a build/CI box has no key). `source` labels the PD event.
#[derive(Debug, Clone, Deserialize)]
pub struct PagerDutyConfig {
    /// Events v2 integration routing key. Empty = PagerDuty disabled. NEVER logged.
    #[serde(default)]
    pub routing_key: String,
    /// PagerDuty `payload.source` (a fixed dedup-independent label).
    #[serde(default = "default_pd_source")]
    pub source: String,
    /// STABLE Events v2 `dedup_key` — a fixed string shared by every
    /// trigger/update/acknowledge/resolve for the active alarm (NOT the per-case
    /// `case_id`). One incident at a time is correct for a single-home alarm, and a
    /// stable key lets HA resolve the page itself (same key) even if Argus is down
    /// — so a klaxon/PD left running with Argus restarted can still be closed.
    #[serde(default = "default_pd_dedup_key")]
    pub dedup_key: String,
}

fn default_pd_source() -> String {
    "argus@atlas".to_string()
}

fn default_pd_dedup_key() -> String {
    "argus-shq-alarm".to_string()
}

/// Real-time offsite replication of the case dir to S3 (Phase 2a).
///
/// An absent `offsite:` block (or `enabled: false`) disables it entirely, hence
/// every field has a `#[serde(default)]`. The two AWS keys come via `${VAR}`
/// env-expansion (handled by [`Config::load`]); they default to empty strings so
/// the config still parses when the secrets are absent in a build/CI context —
/// they're only *required* (and validated) when `enabled` is true.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OffsiteConfig {
    /// Master switch. False (the default) = no S3 client is built, no task spawned.
    #[serde(default)]
    pub enabled: bool,
    /// Destination bucket (estate detail — kept in the private config, not the repo).
    #[serde(default)]
    pub bucket: String,
    /// Away-from-premises region.
    #[serde(default = "default_region")]
    pub region: String,
    /// Key prefix; the case tree mirrors under `<prefix>/<case_id>/...`.
    #[serde(default = "default_prefix")]
    pub prefix: String,
    /// Write-only S3 access key id (`${AWS_ACCESS_KEY_ID}` env-expanded).
    #[serde(default)]
    pub aws_access_key_id: String,
    /// Write-only S3 secret (`${AWS_SECRET_ACCESS_KEY}` env-expanded). Never logged.
    #[serde(default)]
    pub aws_secret_access_key: String,
    /// Upload throttling / retry policy.
    #[serde(default)]
    pub upload: UploadConfig,
}

fn default_region() -> String {
    "ap-southeast-2".to_string()
}

fn default_prefix() -> String {
    "cases".to_string()
}

/// How aggressively the replicator pushes, and how it backs off on failure.
///
/// The three throttle fields below (`best_stills` / `routine_frames` /
/// `routine_sample_secs`) are parsed now so the config shape is stable, but not
/// yet *read*: Phase 2 only persists best stills to the case dir (routine
/// all-camera frames are NOT written to disk), so the throttle is currently a
/// near-no-op — best stills + events are always replicated. They become live when
/// routine-frame sampling is added. Hence `#[allow(dead_code)]` on them.
#[derive(Debug, Clone, Deserialize)]
pub struct UploadConfig {
    /// Best-stills policy: `always` (the evidence is never throttled).
    #[serde(default = "default_best_stills")]
    #[allow(dead_code)]
    pub best_stills: String,
    /// Routine all-camera-frame policy: `always | sampled | never`.
    #[serde(default = "default_routine_frames")]
    #[allow(dead_code)]
    pub routine_frames: String,
    /// Min seconds between sampled routine frames when `routine_frames: sampled`.
    #[serde(default = "default_routine_sample_secs")]
    #[allow(dead_code)]
    pub routine_sample_secs: u64,
    /// Per-pass exponential backoff schedule (seconds) applied after a failing
    /// pass; escalates with consecutive failures, capped at the last value.
    #[serde(default = "default_retry_backoff_secs")]
    pub retry_backoff_secs: Vec<u64>,
    /// Periodic re-scan interval (seconds) — the fallback that catches stills
    /// written between events plus crash/restart recovery, independent of the
    /// `watch` wake signal.
    #[serde(default = "default_scan_interval_secs")]
    pub scan_interval_secs: u64,
}

impl Default for UploadConfig {
    fn default() -> Self {
        Self {
            best_stills: default_best_stills(),
            routine_frames: default_routine_frames(),
            routine_sample_secs: default_routine_sample_secs(),
            retry_backoff_secs: default_retry_backoff_secs(),
            scan_interval_secs: default_scan_interval_secs(),
        }
    }
}

fn default_best_stills() -> String {
    "always".to_string()
}

fn default_routine_frames() -> String {
    "sampled".to_string()
}

fn default_routine_sample_secs() -> u64 {
    30
}

fn default_retry_backoff_secs() -> Vec<u64> {
    vec![2, 5, 15, 60]
}

fn default_scan_interval_secs() -> u64 {
    5
}

/// Assessment-loop cadence and cost guardrails (Phase 2).
#[derive(Debug, Clone, Deserialize)]
pub struct LoopConfig {
    /// Target frame PERIOD in seconds while triggered — the loop aims for one
    /// assessment every `cadence_secs`, padding only when a tick (the per-camera
    /// LLM calls) finished faster; a tick that ran longer fires the next promptly.
    #[serde(default = "default_cadence")]
    pub cadence_secs: u64,
    /// Optional daily billable-input-token cap. When exceeded, the loop slows to
    /// `slow_cadence_secs` until UTC midnight. `null`/absent = no cap.
    #[serde(default)]
    pub daily_token_cap: Option<u64>,
    /// The slow frame period (seconds), used when the daily token cap is hit OR
    /// when the case has gone quiet long enough to decay off Critical (no activity
    /// for ≥ the elevated-decay window) — no point hammering the cameras when
    /// nothing is happening. Fresh activity snaps back to `cadence_secs`.
    #[serde(default = "default_slow_cadence")]
    pub slow_cadence_secs: u64,
    /// Every N ticks, re-sweep ALL cameras regardless of motion (the periodic
    /// full baseline refresh). The first tick of a case is always a full sweep;
    /// `0` = never re-sweep after that baseline (motion-gated forever).
    #[serde(default = "default_full_sweep_every")]
    pub full_sweep_every: u32,
    /// A `_motion`/`_person_detected` sensor that switched `off` within this many
    /// seconds still counts as "recent activity" (debounce window for the gate).
    #[serde(default = "default_motion_window_secs")]
    pub motion_window_secs: u64,
}

impl Default for LoopConfig {
    fn default() -> Self {
        Self {
            cadence_secs: default_cadence(),
            daily_token_cap: None,
            slow_cadence_secs: default_slow_cadence(),
            full_sweep_every: default_full_sweep_every(),
            motion_window_secs: default_motion_window_secs(),
        }
    }
}

fn default_cadence() -> u64 {
    6
}

fn default_slow_cadence() -> u64 {
    30
}

fn default_full_sweep_every() -> u32 {
    5
}

fn default_motion_window_secs() -> u64 {
    20
}

/// Home Assistant connection (localhost on atlas).
#[derive(Debug, Clone, Deserialize)]
pub struct HaConfig {
    /// Base URL, e.g. `http://localhost:8123`.
    pub url: String,
    /// Long-lived access token (minted in the HA UI). Keep in the environment.
    pub token: String,
}

/// Anthropic models + key.
#[derive(Debug, Clone, Deserialize)]
pub struct AnthropicConfig {
    pub api_key: String,
    /// The live what/where loop model (Phase 1 uses this one).
    pub live_model: String,
    /// Reserved for Phase 2 forensic identification (parsed now so the config
    /// shape is stable; not yet read).
    #[serde(default = "default_id_model")]
    #[allow(dead_code)]
    pub id_model: String,
}

fn default_id_model() -> String {
    "claude-opus-4-8".to_string()
}

fn default_timezone() -> String {
    "UTC".to_string()
}

/// A camera Argus can capture, with a human label for the prompt.
#[derive(Debug, Clone, Deserialize)]
pub struct CameraConfig {
    /// HA camera entity id, e.g. `camera.garage_camera_high_resolution_channel`.
    pub entity: String,
    /// Human label included in the prompt, e.g. `Garage`.
    pub label: String,
    /// The room/area this camera watches, for intruder-movement announcements
    /// ("Intruder in <zone>."). Multiple cameras can share a zone — e.g. the two
    /// `Outdoor Living` cameras both map to `Backyard` — so a person crossing
    /// between them is announced once. ABSENT = the zone defaults to the camera
    /// `label` (the 1:1 common case). Align zone names with the HA area names the
    /// trigger-location entity reports, so the initial trigger zone is excluded.
    #[serde(default)]
    pub zone: Option<String>,
    /// The camera's activity `binary_sensor`s (its `_motion` + `_person_detected`).
    /// On a motion-gated tick the camera is included iff any of these is active
    /// (per [`crate::ha::RestClient::sensor_active`]). EMPTY = the camera can't be
    /// motion-gated, so it is ALWAYS included.
    #[serde(default)]
    pub motion_entities: Vec<String>,
}

impl Config {
    /// Load and parse config from `path`, or the default
    /// `~/.config/argus/config.yaml` if `None`. Expands `${VAR}` from the
    /// environment before parsing.
    pub fn load(path: Option<PathBuf>) -> Result<Self> {
        let path = match path {
            Some(p) => p,
            None => default_config_path()?,
        };
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading config {}", path.display()))?;
        let expanded = expand_env(&raw)?;
        let cfg: Config = serde_yaml::from_str(&expanded)
            .with_context(|| format!("parsing config {}", path.display()))?;
        Ok(cfg)
    }

    /// The effective trigger sources (Phase 4a). If `triggers` is configured it is
    /// used verbatim; otherwise the legacy single `alarm_entity` is synthesised
    /// into one `Alarm` source — so the live atlas config (single `alarm_entity`,
    /// no `triggers`) keeps working byte-for-byte as before.
    pub fn trigger_sources(&self) -> Vec<TriggerSourceConfig> {
        if self.triggers.is_empty() {
            vec![TriggerSourceConfig {
                entity: self.alarm_entity.clone(),
                profile: TriggerProfile::Alarm,
            }]
        } else {
            self.triggers.clone()
        }
    }

    /// Map of trigger entity → its [`TriggerProfile`] (from [`trigger_sources`]).
    /// An entity not in the map (e.g. a stray event) defaults to `Alarm` at the
    /// call site. With the legacy config this is exactly
    /// `{ alarm_entity: Alarm }`.
    ///
    /// [`trigger_sources`]: Config::trigger_sources
    pub fn trigger_profile_map(&self) -> HashMap<String, TriggerProfile> {
        self.trigger_sources()
            .into_iter()
            .map(|t| (t.entity, t.profile))
            .collect()
    }

    /// The primary `Alarm`-profile entity — the single alarm panel the HA WS
    /// subscription still filters on in 4a (multi-source firing is 4b). Falls back
    /// to the legacy `alarm_entity` if no source is explicitly tagged `Alarm`.
    pub fn primary_alarm_entity(&self) -> String {
        self.trigger_sources()
            .into_iter()
            .find(|t| t.profile == TriggerProfile::Alarm)
            .map(|t| t.entity)
            .unwrap_or_else(|| self.alarm_entity.clone())
    }

    /// Read the premises seed referenced by `seed_path` (env/`~` expanded).
    pub fn load_seed(&self) -> Result<String> {
        let path = expand_tilde(&self.seed_path);
        std::fs::read_to_string(&path)
            .with_context(|| format!("reading premises seed {}", path.display()))
    }

}

/// `~/.config/argus/config.yaml` (mirrors nyx's `~/.config/shqd/`).
fn default_config_path() -> Result<PathBuf> {
    let dirs = ProjectDirs::from("", "", "argus")
        .context("cannot determine config directory")?;
    Ok(dirs.config_dir().join("config.yaml"))
}

/// Expand a leading `~/` to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(base) = BaseDirs::new() {
            return base.home_dir().join(rest);
        }
    }
    PathBuf::from(path)
}

/// Expand `${VAR}` references from the environment. Errors on an unterminated
/// `${` or a referenced variable that is not set, so a missing secret fails
/// loudly at startup rather than sending an empty token.
///
/// YAML comments are skipped: a `#` at line start or preceded by whitespace
/// begins a comment, and `${...}` inside it is left verbatim (so a literal
/// `${VAR}` in a comment doesn't get treated as a missing env var and abort
/// startup). A `#` not preceded by whitespace (e.g. a URL fragment) is not a
/// comment.
fn expand_env(input: &str) -> Result<String> {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_comment = false;
    let mut prev_ws = true; // line start counts as "preceded by whitespace"
    while let Some(c) = chars.next() {
        if c == '\n' {
            out.push(c);
            in_comment = false;
            prev_ws = true;
            continue;
        }
        if !in_comment && c == '#' && prev_ws {
            in_comment = true;
        }
        if !in_comment && c == '$' && chars.peek() == Some(&'{') {
            chars.next(); // consume '{'
            let mut name = String::new();
            let mut closed = false;
            for nc in chars.by_ref() {
                if nc == '}' {
                    closed = true;
                    break;
                }
                name.push(nc);
            }
            if !closed {
                return Err(anyhow!("unterminated ${{...}} in config"));
            }
            let value = std::env::var(&name)
                .map_err(|_| anyhow!("environment variable {name} (referenced in config) is not set"))?;
            out.push_str(&value);
            prev_ws = false;
        } else {
            out.push(c);
            prev_ws = c.is_whitespace();
        }
    }
    Ok(out)
}
