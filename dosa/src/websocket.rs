use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio::time::{interval, Duration};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::config::ConfigManager;
use crate::door::DoorController;
use crate::messages::{ClientMessage, DoorStatus, ServerMessage};

type ClientId = usize;

/// WebSocket server for door control
pub struct WebSocketServer {
    addr: SocketAddr,
    door: DoorController,
    config_manager: Arc<Mutex<ConfigManager>>,
    clients: Arc<Mutex<HashMap<ClientId, broadcast::Sender<String>>>>,
    next_client_id: Arc<Mutex<ClientId>>,
}

impl WebSocketServer {
    /// Create a new WebSocket server
    pub fn new(addr: SocketAddr, door: DoorController, config_manager: ConfigManager) -> Self {
        Self {
            addr,
            door,
            config_manager: Arc::new(Mutex::new(config_manager)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            next_client_id: Arc::new(Mutex::new(0)),
        }
    }

    /// Start the WebSocket server
    pub async fn start(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        tracing::info!("WebSocket server listening on {}", self.addr);

        // Start periodic status broadcast
        self.start_status_broadcaster();

        loop {
            match listener.accept().await {
                Ok((stream, peer_addr)) => {
                    let server = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = server.handle_connection(stream, peer_addr).await {
                            tracing::error!("Connection error from {}: {}", peer_addr, e);
                        }
                    });
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }

    /// Start background task to broadcast status updates
    fn start_status_broadcaster(&self) {
        let door = self.door.clone();
        let clients = self.clients.clone();
        let mut status_rx = door.subscribe_status();

        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(1));
            let mut last_broadcast_status: Option<DoorStatus> = None;

            // Unified broadcaster: event-driven with fallback polling
            loop {
                tokio::select! {
                    // Priority 1: Event-driven updates from position monitor (immediate)
                    result = status_rx.recv() => {
                        match result {
                            Ok(status) => {
                                // Only broadcast if status actually changed
                                let should_broadcast = match &last_broadcast_status {
                                    None => true,
                                    Some(prev) => prev != &status,
                                };

                                if should_broadcast {
                                    let message = ServerMessage::Status {
                                        version: env!("CARGO_PKG_VERSION").to_string(),
                                        door: status.clone(),
                                    };

                                    if let Ok(json) = serde_json::to_string(&message) {
                                        let clients_lock = clients.lock().await;
                                        for (client_id, tx) in clients_lock.iter() {
                                            if let Err(e) = tx.send(json.clone()) {
                                                tracing::debug!("Failed to broadcast to client {}: {}", client_id, e);
                                            }
                                        }
                                    }

                                    last_broadcast_status = Some(status);
                                }
                            }
                            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                                tracing::warn!("Status broadcaster lagged, skipped {} messages", skipped);
                            }
                            Err(broadcast::error::RecvError::Closed) => {
                                tracing::error!("Status channel closed, stopping broadcaster");
                                break;
                            }
                        }
                    }

                    // Priority 2: Fallback polling for non-movement state changes (every 1 second)
                    _ = ticker.tick() => {
                        let status = door.get_status().await;

                        // Only broadcast if status has changed since last broadcast
                        let should_broadcast = match &last_broadcast_status {
                            None => true,
                            Some(prev) => prev != &status,
                        };

                        if should_broadcast {
                            let message = ServerMessage::Status {
                                version: env!("CARGO_PKG_VERSION").to_string(),
                                door: status.clone(),
                            };

                            if let Ok(json) = serde_json::to_string(&message) {
                                let clients_lock = clients.lock().await;
                                for (client_id, tx) in clients_lock.iter() {
                                    if let Err(e) = tx.send(json.clone()) {
                                        tracing::debug!("Failed to broadcast to client {}: {}", client_id, e);
                                    }
                                }
                            }

                            last_broadcast_status = Some(status);
                        }
                    }
                }
            }
        });
    }

    /// Handle a new client connection
    async fn handle_connection(&self, stream: TcpStream, peer_addr: SocketAddr) -> Result<()> {
        tracing::info!("New connection from {}", peer_addr);

        let ws_stream = accept_async(stream).await?;
        let (mut write, mut read) = ws_stream.split();

        // Register client
        let client_id = self.register_client().await;
        tracing::info!("Client {} registered from {}", client_id, peer_addr);

        // Send initial status
        if let Ok(status_msg) = self.collect_status().await {
            let msg = serde_json::to_string(&status_msg)?;
            let _ = write.send(Message::Text(msg)).await;
        }

        // Get broadcast receiver for this client
        let mut rx = {
            let clients = self.clients.lock().await;
            clients.get(&client_id).unwrap().subscribe()
        };

        loop {
            tokio::select! {
                // Handle incoming messages from client
                msg = read.next() => {
                    match msg {
                        Some(Ok(Message::Text(text))) => {
                            let response = match self.handle_message(&text).await {
                                Ok(resp) => resp,
                                Err(e) => {
                                    // Send error response for invalid messages
                                    tracing::warn!("Invalid message from client {}: {}", client_id, e);
                                    ServerMessage::Error {
                                        message: format!("Invalid command: {}", e),
                                    }
                                }
                            };

                            let response_json = serde_json::to_string(&response)?;
                            write.send(Message::Text(response_json)).await?;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            tracing::info!("Client {} disconnected", client_id);
                            break;
                        }
                        Some(Err(e)) => {
                            tracing::error!("WebSocket error from client {}: {}", client_id, e);
                            break;
                        }
                        _ => {}
                    }
                }
                // Handle broadcast messages to this client
                Ok(broadcast_msg) = rx.recv() => {
                    if let Err(e) = write.send(Message::Text(broadcast_msg)).await {
                        tracing::error!("Failed to send broadcast to client {}: {}", client_id, e);
                        break;
                    }
                }
            }
        }

        // Unregister client
        self.unregister_client(client_id).await;
        tracing::info!("Client {} unregistered", client_id);

        Ok(())
    }

    /// Register a new client
    async fn register_client(&self) -> ClientId {
        let mut next_id = self.next_client_id.lock().await;
        let client_id = *next_id;
        *next_id += 1;

        let (tx, _) = broadcast::channel(100);
        self.clients.lock().await.insert(client_id, tx);

        client_id
    }

    /// Unregister a client
    async fn unregister_client(&self, client_id: ClientId) {
        self.clients.lock().await.remove(&client_id);
    }

    /// Handle a client message
    async fn handle_message(&self, text: &str) -> Result<ServerMessage> {
        let message: ClientMessage = serde_json::from_str(text)?;

        match message {
            ClientMessage::Open => {
                if let Err(e) = self.door.open().await {
                    return Ok(ServerMessage::Error {
                        message: format!("Failed to open door: {}", e),
                    });
                }
                Ok(ServerMessage::Response {
                    success: true,
                    command: "open".to_string(),
                    config: None,
                })
            }
            ClientMessage::Close => {
                if let Err(e) = self.door.close().await {
                    return Ok(ServerMessage::Error {
                        message: format!("Failed to close door: {}", e),
                    });
                }
                Ok(ServerMessage::Response {
                    success: true,
                    command: "close".to_string(),
                    config: None,
                })
            }
            ClientMessage::Move { percent } => {
                if let Err(e) = self.door.move_to_percent(percent).await {
                    return Ok(ServerMessage::Error {
                        message: format!("Failed to move door to {}%: {}", percent, e),
                    });
                }
                Ok(ServerMessage::Response {
                    success: true,
                    command: "move".to_string(),
                    config: None,
                })
            }
            ClientMessage::Home => {
                if let Err(e) = self.door.home().await {
                    return Ok(ServerMessage::Error {
                        message: format!("Failed to home door: {}", e),
                    });
                }
                Ok(ServerMessage::Response {
                    success: true,
                    command: "home".to_string(),
                    config: None,
                })
            }
            ClientMessage::Zero => {
                if let Err(e) = self.door.zero().await {
                    return Ok(ServerMessage::Error {
                        message: format!("Failed to zero door: {}", e),
                    });
                }
                Ok(ServerMessage::Response {
                    success: true,
                    command: "zero".to_string(),
                    config: None,
                })
            }
            ClientMessage::ClearAlarm => {
                if let Err(e) = self.door.clear_alarm().await {
                    return Ok(ServerMessage::Error {
                        message: format!("Failed to clear alarm: {}", e),
                    });
                }
                Ok(ServerMessage::Response {
                    success: true,
                    command: "clear_alarm".to_string(),
                    config: None,
                })
            }
            ClientMessage::Stop => {
                if let Err(e) = self.door.stop().await {
                    return Ok(ServerMessage::Error {
                        message: format!("Failed to stop door: {}", e),
                    });
                }
                Ok(ServerMessage::Response {
                    success: true,
                    command: "stop".to_string(),
                    config: None,
                })
            }
            ClientMessage::Status => {
                // Return cached status (updated in real-time by position monitor and event broadcasts)
                let status = self.door.get_status().await;
                Ok(ServerMessage::Status {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    door: status,
                })
            }
            ClientMessage::SetConfig {
                open_distance,
                open_speed,
                close_speed,
                cnc_axis,
                open_direction,
            } => {
                let mut config = self.door.get_config().await;

                if let Some(dist) = open_distance {
                    config.open_distance = dist;
                }
                if let Some(speed) = open_speed {
                    config.open_speed = speed;
                }
                if let Some(speed) = close_speed {
                    config.close_speed = speed;
                }
                if let Some(axis) = cnc_axis {
                    config.cnc_axis = axis;
                }
                if let Some(dir) = open_direction {
                    config.open_direction = dir;
                }

                self.door.update_config(config.clone()).await;
                self.config_manager
                    .lock()
                    .await
                    .set_door_config(config)
                    .await?;

                Ok(ServerMessage::Response {
                    success: true,
                    command: "set_config".to_string(),
                    config: None,
                })
            }
            ClientMessage::GetConfig => {
                let config = self.door.get_config().await;
                Ok(ServerMessage::Response {
                    success: true,
                    command: "get_config".to_string(),
                    config: Some(config),
                })
            }
            ClientMessage::GetCncSettings => {
                match self.door.query_cnc_settings().await {
                    Ok(settings) => Ok(ServerMessage::CncSettings { settings }),
                    Err(e) => Ok(ServerMessage::Error {
                        message: format!("Failed to query CNC settings: {}", e),
                    }),
                }
            }
            ClientMessage::GetCncSetting { setting } => {
                match self.door.get_cnc_setting(&setting).await {
                    Ok(value) => Ok(ServerMessage::CncSetting { setting, value }),
                    Err(e) => Ok(ServerMessage::Error {
                        message: format!("Failed to get CNC setting {}: {}", setting, e),
                    }),
                }
            }
            ClientMessage::SetCncSetting { setting, value } => {
                match self.door.set_cnc_setting(&setting, &value).await {
                    Ok(()) => Ok(ServerMessage::Response {
                        success: true,
                        command: "set_cnc_setting".to_string(),
                        config: None,
                    }),
                    Err(e) => Ok(ServerMessage::Error {
                        message: format!("Failed to set CNC setting {}={}: {}", setting, value, e),
                    }),
                }
            }
            ClientMessage::Noop => Ok(ServerMessage::Response {
                success: true,
                command: "noop".to_string(),
                config: None,
            }),
        }
    }

    /// Collect and return current status
    async fn collect_status(&self) -> Result<ServerMessage> {
        let status = self.door.get_status().await;

        Ok(ServerMessage::Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            door: status,
        })
    }
}

impl Clone for WebSocketServer {
    fn clone(&self) -> Self {
        Self {
            addr: self.addr,
            door: self.door.clone(),
            config_manager: self.config_manager.clone(),
            clients: self.clients.clone(),
            next_client_id: self.next_client_id.clone(),
        }
    }
}
