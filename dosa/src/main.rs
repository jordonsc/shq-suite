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

/// Initialize the system (config, CNC, door controller)
async fn initialize_system() -> Result<(DoorController, ConfigManager)> {
    // Initialize configuration manager
    let config_manager = ConfigManager::new().await?;
    let door_config = config_manager.get_door_config();

    tracing::info!("Door configuration:");
    tracing::info!("  Open distance: {} mm", door_config.open_distance);
    tracing::info!("  Open speed: {} mm/min", door_config.open_speed);
    tracing::info!("  Close speed: {} mm/min", door_config.close_speed);
    tracing::info!("  CNC axis: {}", door_config.cnc_axis);
    tracing::info!("  Limit offset: {} mm", door_config.limit_offset);
    tracing::info!("  Stop delay: {} ms", door_config.stop_delay_ms);

    // Initialize CNC controller
    let cnc = CncController::new(&door_config.cnc_connection).await?;
    tracing::info!("Connected to CNC controller");

    // Validate stop_delay_ms is sufficient for safe deceleration
    tracing::info!("Validating stop_delay_ms configuration...");
    cnc.validate_stop_delay(
        &door_config.cnc_axis,
        door_config.open_speed,
        door_config.close_speed,
        door_config.stop_delay_ms,
    )
    .await?;

    // Initialize door controller
    let door = DoorController::new(cnc, door_config).await?;
    tracing::info!("Door controller initialized");

    Ok((door, config_manager))
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
        .unwrap_or(8766);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

    // Try to initialize the system - if any error occurs, continue in fault state
    let (door, config_manager) = match initialize_system().await {
        Ok((door, config_manager)) => {
            tracing::info!("System initialized successfully");
            (door, config_manager)
        }
        Err(e) => {
            tracing::error!("System initialization failed: {:?}", e);
            tracing::warn!("Starting in FAULT state - WebSocket API available for status");
            // Load config manager to get the actual configuration
            let config_manager = ConfigManager::new().await
                .unwrap_or_else(|_| panic!("Failed to create config manager"));
            let door_config = config_manager.get_door_config();
            let door = DoorController::new_fault(format!("{:?}", e), door_config);
            (door, config_manager)
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
