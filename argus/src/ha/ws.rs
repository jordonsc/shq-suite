//! HA WebSocket client — authenticates, subscribes to `state_changed`, and
//! emits an `HaEvent::AlarmTriggered` on the transition into `triggered`.
//!
//! Runs forever with reconnect/backoff (the coordinator pattern from
//! `home-assistant/custom_components/shq_display/coordinator.py`). Events are
//! delivered to the caller over an mpsc channel so the assessment work stays
//! decoupled from the socket loop.

use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use futures_util::stream::SplitStream;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tracing::{debug, error, info, warn};

use crate::state::{AlarmState, AlarmTracker};

type WsRead = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Events surfaced from the HA WebSocket stream.
#[derive(Debug, Clone)]
pub enum HaEvent {
    /// The alarm entity transitioned into `triggered`.
    AlarmTriggered { entity_id: String },
}

/// Connect, authenticate, subscribe, and forward alarm-trigger transitions over
/// `tx`. Reconnects forever with exponential backoff; backoff resets after a
/// session that authenticated successfully, so a transient drop reconnects fast
/// while a persistently-down HA backs off.
pub async fn run(ws_url: String, token: String, alarm_entity: String, tx: mpsc::Sender<HaEvent>) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match connect_and_listen(&ws_url, &token, &alarm_entity, &tx).await {
            Ok(()) => {
                warn!("HA WebSocket connection closed; reconnecting");
                backoff = INITIAL_BACKOFF;
            }
            Err(e) => {
                error!("HA WebSocket error: {e:#}");
            }
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn connect_and_listen(
    ws_url: &str,
    token: &str,
    alarm_entity: &str,
    tx: &mpsc::Sender<HaEvent>,
) -> Result<()> {
    info!("Connecting to HA WebSocket at {ws_url}");
    let (stream, _) = connect_async(ws_url).await.context("ws connect")?;
    let (mut write, mut read) = stream.split();

    // 1. Server sends auth_required.
    let first = next_json(&mut read).await.context("awaiting auth_required")?;
    if first["type"] != "auth_required" {
        return Err(anyhow!("expected auth_required, got: {first}"));
    }

    // 2. Reply with the access token.
    write
        .send(Message::Text(
            json!({ "type": "auth", "access_token": token }).to_string(),
        ))
        .await
        .context("sending auth")?;

    // 3. Expect auth_ok.
    let auth = next_json(&mut read).await.context("awaiting auth result")?;
    match auth["type"].as_str() {
        Some("auth_ok") => info!("HA WebSocket authenticated"),
        Some("auth_invalid") => {
            return Err(anyhow!("HA WebSocket auth invalid: {}", auth["message"]))
        }
        other => return Err(anyhow!("unexpected auth response type: {other:?}")),
    }

    // 4. Subscribe to all state_changed events; we filter to the alarm entity.
    write
        .send(Message::Text(
            json!({ "id": 1, "type": "subscribe_events", "event_type": "state_changed" })
                .to_string(),
        ))
        .await
        .context("subscribing to state_changed")?;
    info!("Subscribed to state_changed; watching {alarm_entity}");

    // 5. Process the event stream until the connection drops.
    let mut tracker = AlarmTracker::new();
    while let Some(msg) = read.next().await {
        match msg.context("ws read")? {
            Message::Text(txt) => match serde_json::from_str::<Value>(&txt) {
                Ok(v) => handle_message(&v, alarm_entity, &mut tracker, tx).await,
                Err(e) => warn!("non-JSON WS frame ignored: {e}"),
            },
            Message::Ping(p) => {
                let _ = write.send(Message::Pong(p)).await;
            }
            Message::Close(_) => return Ok(()),
            _ => {}
        }
    }
    Ok(())
}

/// Inspect one decoded WS message; fire on a `state_changed` for the alarm
/// entity that is a transition into `triggered`.
async fn handle_message(
    v: &Value,
    alarm_entity: &str,
    tracker: &mut AlarmTracker,
    tx: &mpsc::Sender<HaEvent>,
) {
    if v["type"] != "event" {
        return;
    }
    let event = &v["event"];
    if event["event_type"] != "state_changed" {
        return;
    }
    let data = &event["data"];
    if data["entity_id"].as_str() != Some(alarm_entity) {
        return;
    }

    let new_state = data["new_state"]["state"].as_str().unwrap_or("unknown");
    debug!("{alarm_entity} state_changed → {new_state}");

    if tracker.update(AlarmState::from_ha(new_state)) {
        info!("{alarm_entity} transitioned to triggered");
        if tx
            .send(HaEvent::AlarmTriggered {
                entity_id: alarm_entity.to_string(),
            })
            .await
            .is_err()
        {
            warn!("event channel closed; dropping trigger");
        }
    }
}

/// Read the next JSON text frame, skipping ping/pong control frames. Used only
/// during the handshake (before the main read loop owns the stream).
async fn next_json(read: &mut WsRead) -> Result<Value> {
    while let Some(msg) = read.next().await {
        match msg? {
            Message::Text(t) => return Ok(serde_json::from_str(&t)?),
            Message::Ping(_) | Message::Pong(_) => continue,
            Message::Close(_) => return Err(anyhow!("connection closed during handshake")),
            _ => continue,
        }
    }
    Err(anyhow!("stream ended during handshake"))
}
