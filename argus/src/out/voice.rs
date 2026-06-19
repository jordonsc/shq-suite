//! Phase 3 — Overwatch gRPC voice client.
//!
//! Drives the klaxon (`SetAlarm`) and speaks positive-only milestones
//! (`Verbalise`) on Overwatch's `VoiceService` (port 50051, plaintext gRPC on the
//! LAN). The proto is `proto/voice.proto` — a symlink to
//! `overwatch/proto/voice.proto`, compiled by `build.rs` into the `voice` module
//! below.
//!
//! **Resilience contract:** every method logs and returns on connect/RPC failure
//! — it NEVER panics and NEVER blocks the PagerDuty channel. A connection is
//! established lazily per call (Overwatch may be rebooting), which is cheap on the
//! LAN and means a flaky link self-heals on the next milestone.
//!
//! > Live-fire is deferred while the residence is asleep: nothing here is called
//! > at build/test time. The methods exist so the wiring compiles and the request
//! > shapes are fixed; the first real `SetAlarm`/`Verbalise` happens on a live
//! > trigger with Overwatch reachable.

/// Generated client stubs from `proto/voice.proto` (client only — `build.rs` sets
/// `build_server(false)`). The package is `voice`.
pub mod proto {
    tonic::include_proto!("voice");
}

use proto::voice_service_client::VoiceServiceClient;
use proto::{SetAlarmRequest, VerbaliseRequest};
use tonic::transport::Channel;
use tracing::{info, warn};

/// A thin handle to the Overwatch voice server. Holds the dial target + voice
/// defaults; connects per call so a rebooting Overwatch never wedges the channel.
#[derive(Clone)]
pub struct VoiceClient {
    endpoint: String,
    alarm_id: String,
    voice_id: String,
    /// Whether the klaxon is sounded at all (`false` = voice-only).
    klaxon_enabled: bool,
    /// `SetAlarm` (klaxon) volume.
    klaxon_volume: f32,
    /// `Verbalise` (spoken-line) volume.
    voice_volume: f32,
    /// Volume to duck a sounding klaxon to while a line plays (`duck_alarm_volume`
    /// on `Verbalise`), so speech stays intelligible over the siren. 0 = no duck.
    duck_volume: f32,
    /// Dry-voice mode (`--dry-voice`): every `SetAlarm`/`Verbalise` is LOGGED at
    /// `info` and Overwatch is never contacted. Lets a live (scratch-alarm) test
    /// show exactly what would be spoken without a real klaxon/voice.
    dry: bool,
}

impl VoiceClient {
    /// Build a client from config. Does NOT connect — the first RPC dials.
    pub fn new(
        host: &str,
        port: u16,
        alarm_id: String,
        voice_id: String,
        klaxon_enabled: bool,
        klaxon_volume: f32,
        voice_volume: f32,
        duck_volume: f32,
    ) -> Self {
        Self {
            endpoint: format!("http://{host}:{port}"),
            alarm_id,
            voice_id,
            klaxon_enabled,
            klaxon_volume,
            voice_volume,
            duck_volume,
            dry: false,
        }
    }

    /// A dry-voice client: speaks nothing, just logs what it WOULD send. Used by
    /// `--dry-voice` so the outputs consumer runs the full voice gate without
    /// touching Overwatch (no live klaxon, no TTS).
    pub fn new_dry() -> Self {
        Self {
            endpoint: String::new(),
            alarm_id: "security".to_string(),
            voice_id: "Amy".to_string(),
            klaxon_enabled: true,
            klaxon_volume: 1.0,
            voice_volume: 1.0,
            duck_volume: 0.15,
            dry: true,
        }
    }

    /// Dial Overwatch. Returns `None` (after logging) on failure — callers treat a
    /// `None` as "skip this RPC", never as fatal.
    async fn connect(&self) -> Option<VoiceServiceClient<Channel>> {
        match VoiceServiceClient::connect(self.endpoint.clone()).await {
            Ok(c) => Some(c),
            Err(e) => {
                warn!("overwatch: cannot connect to {}: {e}", self.endpoint);
                None
            }
        }
    }

    /// Start (or stop) the security klaxon loop via `SetAlarm`.
    /// `enabled: true` starts the loop, `false` stops it. Logs + returns on
    /// failure; never panics.
    pub async fn set_alarm(&self, enabled: bool) {
        if !self.klaxon_enabled {
            // Voice-only mode: never sound the siren (but speech still flows).
            if enabled {
                info!(alarm_id = %self.alarm_id, "klaxon disabled (voice-only); not sounding siren");
            }
            return;
        }
        if self.dry {
            info!(enabled, alarm_id = %self.alarm_id, "[DRY VOICE] would SetAlarm");
            return;
        }
        let Some(mut client) = self.connect().await else {
            return;
        };
        let req = SetAlarmRequest {
            alarm_id: self.alarm_id.clone(),
            enabled,
            volume: Some(self.klaxon_volume),
        };
        match client.set_alarm(req).await {
            Ok(resp) => {
                let r = resp.into_inner();
                info!(
                    enabled,
                    alarm_id = %self.alarm_id,
                    success = r.success,
                    "overwatch: SetAlarm: {}",
                    r.message
                );
            }
            Err(status) => warn!("overwatch: SetAlarm RPC failed: {status}"),
        }
    }

    /// Speak a line via `Verbalise` (AWS Polly TTS on Overwatch). Logs + returns
    /// on failure; never panics. No notification tone is prefixed (positive-only
    /// speech is meant to be calm and authoritative, not alerting).
    pub async fn verbalise(&self, text: &str) {
        if self.dry {
            info!("[DRY VOICE] would speak: {text:?}");
            return;
        }
        let Some(mut client) = self.connect().await else {
            return;
        };
        let req = VerbaliseRequest {
            text: text.to_string(),
            notification_tone_id: None,
            voice_id: Some(self.voice_id.clone()),
            volume: Some(self.voice_volume),
            // Block until Overwatch finishes PLAYING the clip (not just synthesis),
            // so the serial voice worker paces itself and lines never overlap — no
            // local timing estimate needed.
            await_playback: Some(true),
            // Duck any sounding klaxon to this volume for the duration of the line
            // so it stays intelligible over the siren, then Overwatch restores it.
            // Overwatch <0.4.0 ignores this unknown field (no ducking, no error).
            duck_alarm_volume: Some(self.duck_volume),
        };
        match client.verbalise(req).await {
            Ok(resp) => {
                let r = resp.into_inner();
                info!(success = r.success, "overwatch: Verbalise {:?}: {}", text, r.message);
            }
            Err(status) => warn!("overwatch: Verbalise RPC failed: {status}"),
        }
    }
}
