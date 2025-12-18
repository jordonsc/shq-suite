mod cnc;
mod config;
mod door;
mod messages;
mod websocket;

use anyhow::Result;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::signal;

use cnc::CncController;
use config::ConfigManager;
use door::DoorController;
use websocket::WebSocketServer;

/// Initialize the door controller using existing config manager
async fn initialize_door(config_manager: &ConfigManager) -> Result<DoorController> {
    let door_config = config_manager.get_door_config();

    tracing::info!("Door configuration:");
    tracing::info!("  Open distance: {} mm", door_config.open_distance);
    tracing::info!("  Open speed: {} mm/min", door_config.open_speed);
    tracing::info!("  Close speed: {} mm/min", door_config.close_speed);
    tracing::info!("  CNC axis: {}", door_config.cnc_axis);
    tracing::info!("  Open direction: {}", door_config.open_direction);
    tracing::info!("  (Homing pulloff configured via grblHAL $27)");

    // Initialize CNC controller
    let cnc = CncController::new(&door_config.cnc_connection).await?;
    tracing::info!("Connected to CNC controller");

    // Initialize door controller
    let door = DoorController::new(cnc, door_config).await?;
    tracing::info!("Door controller initialized");

    Ok(door)
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "dosa=info".into()),
        )
        .init();

    tracing::info!("Starting DOSA (Door Opening Sensor Automation) v{}", env!("CARGO_PKG_VERSION"));

    // Load configuration
    let config_manager = ConfigManager::new().await?;
    let ws_config = config_manager.get_websocket_config();

    // Parse command-line arguments (can override config values)
    let args: Vec<String> = std::env::args().collect();
    let host = args
        .iter()
        .position(|arg| arg == "--host")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.to_string())
        .unwrap_or(ws_config.host);

    let port = args
        .iter()
        .position(|arg| arg == "--port")
        .and_then(|i| args.get(i + 1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(ws_config.port);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

    // Try to initialize the door - if any error occurs, continue in fault state
    let door = match initialize_door(&config_manager).await {
        Ok(door) => {
            tracing::info!("System initialized successfully");
            door
        }
        Err(e) => {
            tracing::error!("System initialization failed: {:?}", e);
            tracing::warn!("Starting in FAULT state - WebSocket API available for status");
            let door_config = config_manager.get_door_config();
            DoorController::new_fault(format!("{:?}", e), door_config)
        }
    };

    // Create and start WebSocket server
    let server = Arc::new(WebSocketServer::new(addr, door.clone(), config_manager));

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
    server_handle.abort();

    tracing::info!("Shutdown complete");
    Ok(())
}
