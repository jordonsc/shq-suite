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

use std::collections::HashMap;

use crate::case::TriggerProfile;
use crate::state::{AlarmMode, AlarmTracker};

type WsRead = SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>;

const INITIAL_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Events surfaced from the HA WebSocket stream.
#[derive(Debug, Clone)]
pub enum HaEvent {
    /// The alarm entity's coarse mode changed (edge only). The engine maps this to
    /// the case lifecycle (open on `triggered`, stand down on leaving it) AND
    /// broadcasts the mode for the kiosk HUD (arming/armed → standby pane).
    AlarmModeChanged { entity_id: String, mode: AlarmMode },
    /// A SOFTER (non-alarm) trigger source went active (Phase 4b) — e.g. a
    /// perimeter group (`Investigate`) or a front-door approach (`General`). The
    /// engine opens a GATED case with the source's [`TriggerProfile`]. Unlike the
    /// alarm entity, only the active (on/detected) EDGE matters here — there is no
    /// arm/disarm state machine for these sources.
    TriggerFired {
        entity_id: String,
        profile: TriggerProfile,
    },
}

/// Connect, authenticate, subscribe, and forward alarm-trigger transitions over
/// `tx`. Reconnects forever with exponential backoff; backoff resets after a
/// session that authenticated successfully, so a transient drop reconnects fast
/// while a persistently-down HA backs off.
///
/// `alarm_entity` is the primary `Alarm`-profile panel, driving the full
/// arm/disarm `AlarmMode` state machine. `softer_triggers` (Phase 4b) maps each
/// NON-alarm trigger entity to its [`TriggerProfile`]; a softer source going
/// active (on/detected) fires a one-shot `HaEvent::TriggerFired` that opens a
/// gated case. The softer map is typically empty (the live atlas config has only
/// the alarm entity) — those entities are user-owned and wired at test time.
pub async fn run(
    ws_url: String,
    token: String,
    alarm_entity: String,
    softer_triggers: HashMap<String, TriggerProfile>,
    tx: mpsc::Sender<HaEvent>,
) {
    let mut backoff = INITIAL_BACKOFF;
    loop {
        match connect_and_listen(&ws_url, &token, &alarm_entity, &softer_triggers, &tx).await {
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
    softer_triggers: &HashMap<String, TriggerProfile>,
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
    if softer_triggers.is_empty() {
        info!("Subscribed to state_changed; watching {alarm_entity}");
    } else {
        info!(
            "Subscribed to state_changed; watching {alarm_entity} + {} softer trigger(s)",
            softer_triggers.len()
        );
    }

    // 5. Process the event stream until the connection drops.
    let mut tracker = AlarmTracker::new();
    // Per-softer-entity last-seen "active" flag, so we fire only on the
    // inactive→active EDGE (not on every attribute-only repeat while active).
    let mut softer_active: HashMap<String, bool> = HashMap::new();
    while let Some(msg) = read.next().await {
        match msg.context("ws read")? {
            Message::Text(txt) => match serde_json::from_str::<Value>(&txt) {
                Ok(v) => {
                    handle_message(
                        &v,
                        alarm_entity,
                        softer_triggers,
                        &mut tracker,
                        &mut softer_active,
                        tx,
                    )
                    .await
                }
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

/// Inspect one decoded WS message. For the alarm entity: fire the coarse mode on
/// every change-edge (the full state machine). For a SOFTER trigger entity (Phase
/// 4b): fire a one-shot `TriggerFired` on the inactive→active edge.
async fn handle_message(
    v: &Value,
    alarm_entity: &str,
    softer_triggers: &HashMap<String, TriggerProfile>,
    tracker: &mut AlarmTracker,
    softer_active: &mut HashMap<String, bool>,
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
    let Some(entity_id) = data["entity_id"].as_str() else {
        return;
    };
    let new_state = data["new_state"]["state"].as_str().unwrap_or("unknown");

    // ── Primary alarm panel: the full arm/disarm mode machine ────────────────
    if entity_id == alarm_entity {
        debug!("{alarm_entity} state_changed → {new_state}");
        let mode = match tracker.update(AlarmMode::from_ha(new_state)) {
            Some(m) => m,
            None => return, // same-mode repeat (attribute-only change)
        };
        info!("{alarm_entity} mode → {} ({new_state})", mode.as_str());
        let event = HaEvent::AlarmModeChanged {
            entity_id: alarm_entity.to_string(),
            mode,
        };
        if tx.send(event).await.is_err() {
            warn!("event channel closed; dropping alarm transition");
        }
        return;
    }

    // ── Softer (non-alarm) trigger source: fire on the inactive→active edge ──
    if let Some(&profile) = softer_triggers.get(entity_id) {
        let active = is_active(new_state);
        let was_active = softer_active.insert(entity_id.to_string(), active).unwrap_or(false);
        if active && !was_active {
            info!("softer trigger {entity_id} active ({new_state}) → {profile:?} case");
            let event = HaEvent::TriggerFired {
                entity_id: entity_id.to_string(),
                profile,
            };
            if tx.send(event).await.is_err() {
                warn!("event channel closed; dropping softer trigger");
            }
        }
    }
}

/// Whether a softer-trigger entity's raw state counts as ACTIVE (fires a case). A
/// binary/smart-detect sensor reads `on`/`detected`; placeholder/off states do
/// not. Anything not clearly active (incl. `unavailable`/`unknown`) is inactive.
fn is_active(state: &str) -> bool {
    matches!(
        state.trim().to_ascii_lowercase().as_str(),
        "on" | "detected" | "true" | "open" | "home"
    )
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
