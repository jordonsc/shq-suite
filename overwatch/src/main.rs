mod audio;
mod config;
mod service;
mod tts;

use config::Config;
use service::voice::voice_service_server::VoiceServiceServer;
use service::VoiceServiceImpl;
use tonic::transport::Server;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "overwatch=info,tower_http=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Load configuration
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.yaml".to_string());
    tracing::info!("Loading configuration from: {}", config_path);
    let config = Config::from_file(&config_path)?;

    let server_address = config.server_address.clone();

    // Create service
    tracing::info!("Initializing voice service...");
    let voice_service = VoiceServiceImpl::new(config).await?;

    // Parse server address
    let addr = server_address.parse()?;

    tracing::info!("Starting gRPC server on {}", addr);

    // Start server
    Server::builder()
        .add_service(VoiceServiceServer::new(voice_service))
        .serve(addr)
        .await?;

    Ok(())
}
