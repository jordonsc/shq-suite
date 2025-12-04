use aws_sdk_polly::types::{Engine, OutputFormat, VoiceId};
use aws_sdk_polly::Client as PollyClient;
use aws_config::BehaviorVersion;
use crate::config::AwsConfig;
use sha2::{Sha256, Digest};
use std::path::PathBuf;

pub struct TtsService {
    client: PollyClient,
    cache_dir: PathBuf,
}

impl TtsService {
    pub async fn new(aws_config: Option<&AwsConfig>) -> Self {
        let config = if let Some(aws_cfg) = aws_config {
            let mut loader = aws_config::defaults(BehaviorVersion::latest());

            if let Some(region) = &aws_cfg.region {
                loader = loader.region(aws_config::Region::new(region.clone()));
            }

            if let Some(access_key) = &aws_cfg.access_key_id {
                if let Some(secret_key) = &aws_cfg.secret_access_key {
                    loader = loader.credentials_provider(
                        aws_sdk_polly::config::Credentials::new(
                            access_key,
                            secret_key,
                            None,
                            None,
                            "config-file",
                        ),
                    );
                }
            }

            loader.load().await
        } else {
            aws_config::load_from_env().await
        };

        let client = PollyClient::new(&config);

        // Set up cache directory
        let cache_dir = PathBuf::from("./cache/tts");
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            tracing::warn!("Failed to create TTS cache directory: {}", e);
        }

        Self { client, cache_dir }
    }

    pub async fn synthesize(
        &self,
        text: &str,
        voice_name: &str,
        engine_name: &str,
    ) -> anyhow::Result<Vec<u8>> {
        // Generate cache key from voice, engine, and text
        let cache_key = self.generate_cache_key(text, voice_name, engine_name);

        // Check cache first
        if let Some(cached_data) = self.load_from_cache(&cache_key) {
            return Ok(cached_data);
        }

        // Cache miss - synthesize using AWS Polly
        let voice_id = self.parse_voice_id(voice_name)?;
        let engine = self.parse_engine(engine_name)?;

        tracing::info!(
            "Synthesizing speech via AWS Polly: voice={}, engine={:?}, text_length={}",
            voice_name,
            engine,
            text.len()
        );

        let response = match self
            .client
            .synthesize_speech()
            .engine(engine.clone())
            .output_format(OutputFormat::Mp3)
            .text(text)
            .voice_id(voice_id.clone())
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) => {
                tracing::error!(
                    "AWS Polly synthesis failed: voice={}, engine={:?}, error={:?}",
                    voice_name,
                    engine,
                    e
                );
                return Err(anyhow::anyhow!(
                    "AWS Polly error for voice '{}' with engine '{:?}': {}",
                    voice_name,
                    engine,
                    e
                ));
            }
        };

        match response.audio_stream.collect().await {
            Ok(audio_stream) => {
                let bytes = audio_stream.into_bytes().to_vec();
                tracing::info!("Successfully synthesized {} bytes of audio", bytes.len());

                // Save to cache (ignore errors - caching is non-critical)
                if let Err(e) = self.save_to_cache(&cache_key, &bytes) {
                    tracing::warn!("Failed to save to TTS cache: {}", e);
                }

                Ok(bytes)
            }
            Err(e) => {
                tracing::error!("Failed to collect audio stream: {:?}", e);
                Err(anyhow::anyhow!("Failed to collect audio stream: {}", e))
            }
        }
    }

    fn parse_engine(&self, engine_name: &str) -> anyhow::Result<Engine> {
        match engine_name.to_lowercase().as_str() {
            "neural" => Ok(Engine::Neural),
            "generative" => Ok(Engine::Generative),
            "long-form" | "longform" => Ok(Engine::LongForm),
            "standard" => Ok(Engine::Standard),
            _ => Err(anyhow::anyhow!(
                "Unsupported engine: {}. Valid options: neural, generative, long-form, standard",
                engine_name
            )),
        }
    }

    fn parse_voice_id(&self, voice_name: &str) -> anyhow::Result<VoiceId> {
        match voice_name.to_lowercase().as_str() {
            // US English
            "danielle" => Ok(VoiceId::Danielle),
            "gregory" => Ok(VoiceId::Gregory),
            "ivy" => Ok(VoiceId::Ivy),
            "joanna" => Ok(VoiceId::Joanna),
            "kendra" => Ok(VoiceId::Kendra),
            "kimberly" => Ok(VoiceId::Kimberly),
            "salli" => Ok(VoiceId::Salli),
            "joey" => Ok(VoiceId::Joey),
            "justin" => Ok(VoiceId::Justin),
            "kevin" => Ok(VoiceId::Kevin),
            "matthew" => Ok(VoiceId::Matthew),
            "ruth" => Ok(VoiceId::Ruth),
            "stephen" => Ok(VoiceId::Stephen),
            //"patrick" => Ok(VoiceId::Patrick),

            // British English
            "amy" => Ok(VoiceId::Amy),
            "emma" => Ok(VoiceId::Emma),
            "brian" => Ok(VoiceId::Brian),
            "arthur" => Ok(VoiceId::Arthur),

            // Australian English
            "nicole" => Ok(VoiceId::Nicole),
            "olivia" => Ok(VoiceId::Olivia),
            "russell" => Ok(VoiceId::Russell),

            // Indian English
            "aditi" => Ok(VoiceId::Aditi),
            "raveena" => Ok(VoiceId::Raveena),
            "kajal" => Ok(VoiceId::Kajal),

            // Irish English
            "niamh" => Ok(VoiceId::Niamh),

            // New Zealand English
            "aria" => Ok(VoiceId::Aria),

            // Singaporean English
            "jasmine" => Ok(VoiceId::Jasmine),

            // South African English
            "ayanda" => Ok(VoiceId::Ayanda),

            // Welsh English
            "geraint" => Ok(VoiceId::Geraint),

            _ => Err(anyhow::anyhow!("Unsupported voice: {}", voice_name)),
        }
    }

    fn generate_cache_key(&self, text: &str, voice_name: &str, engine_name: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(text.as_bytes());
        hasher.update(voice_name.as_bytes());
        hasher.update(engine_name.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)
    }

    fn get_cache_path(&self, cache_key: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.mp3", cache_key))
    }

    fn load_from_cache(&self, cache_key: &str) -> Option<Vec<u8>> {
        let cache_path = self.get_cache_path(cache_key);
        match std::fs::read(&cache_path) {
            Ok(data) => {
                tracing::info!("TTS cache hit: {} ({} bytes)", cache_key, data.len());
                Some(data)
            }
            Err(_) => {
                tracing::debug!("TTS cache miss: {}", cache_key);
                None
            }
        }
    }

    fn save_to_cache(&self, cache_key: &str, data: &[u8]) -> anyhow::Result<()> {
        let cache_path = self.get_cache_path(cache_key);
        std::fs::write(&cache_path, data)?;
        tracing::info!("Saved to TTS cache: {} ({} bytes)", cache_key, data.len());
        Ok(())
    }
}
