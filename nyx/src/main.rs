mod auto_dim;
mod config;
mod display;
mod messages;
mod touch;
mod websocket;

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::signal;

use auto_dim::AutoDimManager;
use config::ConfigManager;
use display::DisplayController;
use touch::TouchMonitor;
use websocket::WebSocketServer;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "nyx=info".into()),
        )
        .init();

    tracing::info!("Starting Nyx Display Server v{}", env!("CARGO_PKG_VERSION"));

    // Parse command-line arguments
    let args: Vec<String> = std::env::args().collect();
    let host = args
        .iter()
        .position(|arg| arg == "--host")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("0.0.0.0");

    let port = args
        .iter()
        .position(|arg| arg == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(8765);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

    // Initialize configuration manager
    let config_manager = ConfigManager::new().await?;
    let auto_dim_config = config_manager.get_auto_dim_config();

    // Initialize display controller
    let display = DisplayController::new().await?;

    // Initialize touch monitor
    let touch_monitor = TouchMonitor::new();
    touch_monitor.start().await?;

    // Initialize auto-dim manager
    let auto_dim = AutoDimManager::new(auto_dim_config, display.clone(), touch_monitor.clone());
    auto_dim.start().await?;

    // Set display to bright level on startup
    let config = auto_dim.get_config().await;
    if let Err(e) = display.set_brightness(config.bright_level).await {
        tracing::warn!("Failed to set initial brightness: {}", e);
    }

    // Create and start WebSocket server
    let server = Arc::new(WebSocketServer::new(
        addr,
        display.clone(),
        auto_dim.clone(),
        config_manager,
    ));

    // Spawn server task
    let server_clone = server.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = server_clone.start().await {
            tracing::error!("WebSocket server error: {}", e);
        }
    });

    // Wait for shutdown signal
    match signal::ctrl_c().await {
        Ok(()) => {
            tracing::info!("Received shutdown signal");
        }
        Err(err) => {
            tracing::error!("Unable to listen for shutdown signal: {}", err);
        }
    }

    // Cleanup
    tracing::info!("Shutting down...");
    auto_dim.stop();
    touch_monitor.stop();
    server_handle.abort();

    tracing::info!("Shutdown complete");
    Ok(())
}
