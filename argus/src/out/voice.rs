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
    volume: f32,
}

impl VoiceClient {
    /// Build a client from config. Does NOT connect — the first RPC dials.
    pub fn new(host: &str, port: u16, alarm_id: String, voice_id: String, volume: f32) -> Self {
        Self {
            endpoint: format!("http://{host}:{port}"),
            alarm_id,
            voice_id,
            volume,
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
        let Some(mut client) = self.connect().await else {
            return;
        };
        let req = SetAlarmRequest {
            alarm_id: self.alarm_id.clone(),
            enabled,
            volume: Some(self.volume),
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
        let Some(mut client) = self.connect().await else {
            return;
        };
        let req = VerbaliseRequest {
            text: text.to_string(),
            notification_tone_id: None,
            voice_id: Some(self.voice_id.clone()),
            volume: Some(self.volume),
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
