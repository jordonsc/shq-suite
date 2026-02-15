use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, Mutex};
use tokio_tungstenite::{accept_async, tungstenite::Message};

use crate::auto_dim::AutoDimManager;
use crate::cdp;
use crate::config::ConfigManager;
use crate::display::DisplayController;
use crate::messages::{AutoDimConfig, ClientMessage, ServerMessage};

type ClientId = usize;

/// WebSocket server for display control
pub struct WebSocketServer {
    addr: SocketAddr,
    display: DisplayController,
    auto_dim: AutoDimManager,
    config_manager: Arc<Mutex<ConfigManager>>,
    clients: Arc<Mutex<HashMap<ClientId, broadcast::Sender<String>>>>,
    next_client_id: Arc<Mutex<ClientId>>,
}

impl WebSocketServer {
    /// Create a new WebSocket server
    pub fn new(
        addr: SocketAddr,
        display: DisplayController,
        auto_dim: AutoDimManager,
        config_manager: ConfigManager,
    ) -> Self {
        Self {
            addr,
            display,
            auto_dim,
            config_manager: Arc::new(Mutex::new(config_manager)),
            clients: Arc::new(Mutex::new(HashMap::new())),
            next_client_id: Arc::new(Mutex::new(0)),
        }
    }

    /// Start the WebSocket server
    pub async fn start(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(self.addr).await?;
        tracing::info!("WebSocket server listening on {}", self.addr);

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

    /// Handle a new client connection
    async fn handle_connection(&self, stream: TcpStream, peer_addr: SocketAddr) -> Result<()> {
        tracing::info!("New connection from {}", peer_addr);

        let ws_stream = accept_async(stream).await?;
        let (mut write, mut read) = ws_stream.split();

        // Register client
        let client_id = self.register_client().await;
        tracing::info!("Client {} registered from {}", client_id, peer_addr);

        // Send initial metrics
        if let Ok(metrics) = self.collect_metrics().await {
            let msg = serde_json::to_string(&metrics)?;
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
                                Err(e) => ServerMessage::Error {
                                    message: format!("Invalid message: {}", e),
                                },
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

    /// Broadcast a message to all connected clients
    pub async fn broadcast(&self, message: &ServerMessage) -> Result<()> {
        let json = serde_json::to_string(message)?;
        let clients = self.clients.lock().await;

        for (client_id, tx) in clients.iter() {
            if let Err(e) = tx.send(json.clone()) {
                tracing::warn!("Failed to broadcast to client {}: {}", client_id, e);
            }
        }

        Ok(())
    }

    /// Handle a client message
    async fn handle_message(&self, text: &str) -> Result<ServerMessage> {
        let message: ClientMessage = serde_json::from_str(text)?;

        match message {
            ClientMessage::SetDisplay { state } => {
                self.display.set_display_state(state).await?;
                self.auto_dim.reset_dimmed_state().await;
                self.broadcast_metrics().await;
                Ok(ServerMessage::Response {
                    success: true,
                    command: "set_display".to_string(),
                    config: None,
                    url: None,
                })
            }
            ClientMessage::SetBrightness { brightness } => {
                if brightness == 0 {
                    // Setting brightness to 0 is same as sleep
                    self.auto_dim.sleep().await?;
                } else {
                    self.display.set_brightness(brightness).await?;
                    self.auto_dim.reset_dimmed_state().await;
                }
                self.broadcast_metrics().await;
                Ok(ServerMessage::Response {
                    success: true,
                    command: "set_brightness".to_string(),
                    config: None,
                    url: None,
                })
            }
            ClientMessage::GetMetrics => self.collect_metrics().await,
            ClientMessage::SetAutoDimConfig {
                dim_level,
                bright_level,
                auto_dim_time,
                auto_off_time,
            } => {
                if bright_level == 0 {
                    return Ok(ServerMessage::Error {
                        message: "bright_level must be greater than 0 (use dim_level for dimmed brightness)".to_string(),
                    });
                }

                let config = AutoDimConfig {
                    dim_level,
                    bright_level,
                    auto_dim_time,
                    auto_off_time,
                };

                self.auto_dim.set_config(config.clone()).await;
                self.config_manager
                    .lock()
                    .await
                    .set_auto_dim_config(config)
                    .await?;

                self.broadcast_metrics().await;

                Ok(ServerMessage::Response {
                    success: true,
                    command: "set_auto_dim_config".to_string(),
                    config: None,
                    url: None,
                })
            }
            ClientMessage::GetAutoDimConfig => {
                let config = self.auto_dim.get_config().await;
                Ok(ServerMessage::Response {
                    success: true,
                    command: "get_auto_dim_config".to_string(),
                    config: Some(config),
                    url: None,
                })
            }
            ClientMessage::Wake => {
                self.auto_dim.wake().await?;
                self.broadcast_metrics().await;
                Ok(ServerMessage::Response {
                    success: true,
                    command: "wake".to_string(),
                    config: None,
                    url: None,
                })
            }
            ClientMessage::Sleep => {
                self.auto_dim.sleep().await?;
                self.broadcast_metrics().await;
                Ok(ServerMessage::Response {
                    success: true,
                    command: "sleep".to_string(),
                    config: None,
                    url: None,
                })
            }
            ClientMessage::Navigate { url } => {
                match cdp::navigate(&url).await {
                    Ok(()) => {
                        tracing::info!("Navigated Chrome to {}", url);
                        self.broadcast_metrics().await;
                        Ok(ServerMessage::Response {
                            success: true,
                            command: "navigate".to_string(),
                            config: None,
                            url: Some(url),
                        })
                    }
                    Err(e) => {
                        tracing::error!("Failed to navigate: {:#}", e);
                        Ok(ServerMessage::Error {
                            message: format!("Navigate failed: {:#}", e),
                        })
                    }
                }
            }
            ClientMessage::GetUrl => {
                match cdp::get_current_url().await {
                    Ok(url) => Ok(ServerMessage::Response {
                        success: true,
                        command: "get_url".to_string(),
                        config: None,
                        url: Some(url),
                    }),
                    Err(e) => {
                        tracing::error!("Failed to get URL: {:#}", e);
                        Ok(ServerMessage::Error {
                            message: format!("Get URL failed: {:#}", e),
                        })
                    }
                }
            }
            ClientMessage::Noop => Ok(ServerMessage::Response {
                success: true,
                command: "noop".to_string(),
                config: None,
                url: None,
            }),
        }
    }

    /// Collect and return current metrics
    async fn collect_metrics(&self) -> Result<ServerMessage> {
        let display = self.display.get_metrics().await?;
        let auto_dim = self.auto_dim.get_status().await;
        let url = cdp::get_current_url().await.ok();

        Ok(ServerMessage::Metrics {
            version: env!("CARGO_PKG_VERSION").to_string(),
            display,
            auto_dim,
            url,
        })
    }

    /// Broadcast current metrics to all clients
    async fn broadcast_metrics(&self) {
        if let Ok(metrics) = self.collect_metrics().await {
            let _ = self.broadcast(&metrics).await;
        }
    }
}
