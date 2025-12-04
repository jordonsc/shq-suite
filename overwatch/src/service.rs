use crate::audio::AudioManager;
use crate::config::Config;
use crate::tts::TtsService;
use std::sync::Arc;
use tonic::{Request, Response, Status};

pub mod voice {
    tonic::include_proto!("voice");
}

use voice::voice_service_server::VoiceService;
use voice::{
    SetAlarmRequest, SetAlarmResponse, VerbaliseRequest, VerbaliseResponse,
};

pub struct VoiceServiceImpl {
    config: Arc<Config>,
    audio_manager: Arc<AudioManager>,
    tts_service: Arc<TtsService>,
}

impl VoiceServiceImpl {
    pub async fn new(config: Config) -> anyhow::Result<Self> {
        let audio_manager = AudioManager::new()?;
        let tts_service = TtsService::new(config.aws.as_ref()).await;

        Ok(Self {
            config: Arc::new(config),
            audio_manager: Arc::new(audio_manager),
            tts_service: Arc::new(tts_service),
        })
    }
}

#[tonic::async_trait]
impl VoiceService for VoiceServiceImpl {
    async fn set_alarm(
        &self,
        request: Request<SetAlarmRequest>,
    ) -> Result<Response<SetAlarmResponse>, Status> {
        let req = request.into_inner();
        let alarm_id = req.alarm_id;
        let enabled = req.enabled;

        tracing::info!("Setting alarm '{}' enabled={}", alarm_id, enabled);

        let alarm_config = self
            .config
            .get_alarm(&alarm_id)
            .ok_or_else(|| Status::not_found(format!("Alarm '{}' not found", alarm_id)))?;

        // Determine volume to use (either specified or default)
        let volume = req.volume.unwrap_or(self.config.default_volume);

        // Validate volume range (0.0 to 2.0)
        if volume < 0.0 || volume > 2.0 {
            return Err(Status::invalid_argument(
                format!("Volume must be between 0.0 and 2.0, got {}", volume)
            ));
        }

        if volume > 1.0 {
            tracing::warn!("Volume {} exceeds 1.0, may cause audio clipping", volume);
        }

        let result = if enabled {
            // Start the alarm
            match self
                .audio_manager
                .start_alarm(alarm_id.clone(), alarm_config.clone(), volume)
                .await
            {
                Ok(_) => SetAlarmResponse {
                    success: true,
                    message: format!("Alarm '{}' started", alarm_id),
                },
                Err(e) => SetAlarmResponse {
                    success: false,
                    message: format!("Failed to start alarm: {}", e),
                },
            }
        } else {
            // Stop the alarm
            let stopped = self.audio_manager.stop_alarm(alarm_id.clone()).await;
            SetAlarmResponse {
                success: true,
                message: if stopped {
                    format!("Alarm '{}' stopped", alarm_id)
                } else {
                    format!("Alarm '{}' was not playing", alarm_id)
                },
            }
        };

        Ok(Response::new(result))
    }

    async fn verbalise(
        &self,
        request: Request<VerbaliseRequest>,
    ) -> Result<Response<VerbaliseResponse>, Status> {
        let req = request.into_inner();
        let text = req.text;
        let notification_tone_id = req.notification_tone_id;
        let voice_id = req.voice_id;

        tracing::info!(
            "Verbalising text: '{}' with tone={:?}, voice={:?}",
            text,
            notification_tone_id,
            voice_id
        );

        // Determine voice to use (either specified or default)
        let voice_name = voice_id.unwrap_or_else(|| self.config.default_voice.clone());

        // Determine volume to use (either specified or default)
        let volume = req.volume.unwrap_or(self.config.default_volume);

        // Validate volume range (0.0 to 2.0)
        if volume < 0.0 || volume > 2.0 {
            return Err(Status::invalid_argument(
                format!("Volume must be between 0.0 and 2.0, got {}", volume)
            ));
        }

        if volume > 1.0 {
            tracing::warn!("Volume {} exceeds 1.0, may cause audio clipping", volume);
        }

        // Start TTS synthesis immediately (in parallel with notification tone)
        tracing::info!(
            "Starting TTS synthesis: voice='{}', engine='{}', text_length={}",
            voice_name,
            self.config.default_engine,
            text.len()
        );

        let tts_service = Arc::clone(&self.tts_service);
        let text_clone = text.clone();
        let voice_name_clone = voice_name.clone();
        let engine_clone = self.config.default_engine.clone();

        let synthesis_task = tokio::spawn(async move {
            tts_service
                .synthesize(&text_clone, &voice_name_clone, &engine_clone)
                .await
        });

        // Play notification tone while synthesis is happening
        if let Some(tone_id) = notification_tone_id {
            if let Some(tone_path) = self.config.get_notification_tone(&tone_id) {
                if let Err(e) = self
                    .audio_manager
                    .play_file(tone_path.clone(), volume)
                    .await
                {
                    tracing::warn!("Failed to play notification tone: {}", e);
                }
                // Small delay to let the tone finish playing
                tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
            } else {
                tracing::warn!("Notification tone '{}' not found", tone_id);
            }
        }

        // Wait for TTS synthesis to complete
        let audio_data = synthesis_task
            .await
            .map_err(|e| Status::internal(format!("TTS synthesis task failed: {}", e)))?
            .map_err(|e| {
                tracing::error!(
                    "TTS synthesis failed: voice='{}', engine='{}', error={}",
                    voice_name,
                    self.config.default_engine,
                    e
                );
                Status::internal(format!(
                    "TTS synthesis failed for voice '{}' with engine '{}': {}",
                    voice_name, self.config.default_engine, e
                ))
            })?;

        // Play synthesized audio
        self.audio_manager
            .play_bytes(audio_data, volume)
            .await
            .map_err(|e| Status::internal(format!("Audio playback failed: {}", e)))?;

        let response = VerbaliseResponse {
            success: true,
            message: "Speech synthesised and played successfully".to_string(),
        };

        Ok(Response::new(response))
    }
}
