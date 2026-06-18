//! Phase 4 — the kiosk HUD HTTP/WS server (`axum`).
//!
//! Serves three surfaces off the Phase 2 `CaseState` broadcast + the on-disk
//! case dir; none of it calls the LLM or HA (it renders/serves `CaseState`):
//!
//! - `GET /alarm` (and `/`) → the static HUD app (`web.dir`, served by
//!   tower-http `ServeDir`; `index.html` is the SPA entry). The frontend agent
//!   owns the contents of that dir — we only serve it at runtime.
//! - `GET /stills/:id` → the JPEG bytes for still `<id>` under the CURRENT
//!   case's `stills/<id>.jpg`. The current case id comes from the latest
//!   `CaseState` on the watch channel; if there is no active case (or the still
//!   isn't under it) we fall back to scanning every `cases/*/stills/<id>.jpg`
//!   (still ids are unique within a case but a just-cleared case can still be
//!   on screen, so the scan is the robust path). 404 if nowhere.
//! - `GET /kiosk` → a WebSocket. On connect we immediately send the current
//!   `CaseState` JSON (or the literal `null` if none), then push the full
//!   `CaseState` JSON on every change. Reconnect is the client's job; the
//!   server only pushes. The pushed JSON is the SAME serde serialisation
//!   journalled to disk — one schema, three surfaces.
//!
//! The server binds on the LAN (`web.bind`, default `0.0.0.0:8770`) so kiosks
//! reach `http://atlas:<port>/alarm`. Absent `web:` config = this is never
//! spawned (see `main.rs`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket},
        Path as AxumPath, State, WebSocketUpgrade,
    },
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tokio::sync::watch;
use tower_http::services::ServeDir;
use tracing::{debug, info, warn};

use crate::case::CaseState;
use crate::config::WebConfig;

/// Shared server state: the case broadcast receiver (cloned per WS connection)
/// and the on-disk case base (`~/.local/share/argus`) for resolving stills.
#[derive(Clone)]
struct AppState {
    rx: watch::Receiver<Option<CaseState>>,
    case_base: Arc<PathBuf>,
}

/// Spawn the HUD server. Runs until the process exits (or the bind fails, which
/// is logged — a HUD failure must not take the daemon down).
pub async fn serve(cfg: WebConfig, rx: watch::Receiver<Option<CaseState>>, case_base: PathBuf) {
    let dir = resolve_web_dir(&cfg.dir);
    if !dir.join("index.html").exists() {
        warn!(
            dir = %dir.display(),
            "HUD web dir has no index.html yet (the frontend may still be in flight); \
             serving the dir anyway — /alarm will 404 until index.html lands"
        );
    } else {
        info!(dir = %dir.display(), "Serving HUD app");
    }

    let state = AppState {
        rx,
        case_base: Arc::new(case_base),
    };

    // Static assets: ServeDir serves `/` → index.html and every asset under the
    // web dir (css/js/images). `/alarm` is routed to the same index.html so the
    // SPA is the deliberate landing page; the kiosk navigates to `<base>/alarm`.
    // `index.html` missing at runtime just yields a 404 (warned above) — the
    // frontend agent owns that file.
    let assets = ServeDir::new(&dir).append_index_html_on_directories(true);
    let alarm = ServeDir::new(&dir).append_index_html_on_directories(true);

    let app = Router::new()
        .route("/kiosk", get(kiosk_ws))
        .route("/stills/:id", get(still))
        .nest_service("/alarm", alarm)
        .fallback_service(assets)
        .with_state(state);

    let bind = cfg.bind.clone();
    let listener = match tokio::net::TcpListener::bind(&bind).await {
        Ok(l) => l,
        Err(e) => {
            warn!(bind = %bind, error = %e, "HUD server failed to bind; HUD disabled");
            return;
        }
    };
    info!(bind = %bind, "HUD server listening (GET /alarm, /stills/:id, WS /kiosk)");
    if let Err(e) = axum::serve(listener, app).await {
        warn!(error = %e, "HUD server exited");
    }
}

/// Resolve the configured web dir. Absolute paths are used verbatim; a relative
/// path resolves against the argus binary's own directory (so a deployed `web/`
/// next to the binary works), falling back to the cwd-relative path if the
/// binary location can't be determined.
fn resolve_web_dir(dir: &str) -> PathBuf {
    let p = PathBuf::from(dir);
    if p.is_absolute() {
        return p;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            return parent.join(&p);
        }
    }
    p
}

/// `GET /kiosk` → WebSocket. Push the current `CaseState` on connect, then the
/// full `CaseState` on every change.
async fn kiosk_ws(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| kiosk_socket(socket, state.rx.clone()))
}

async fn kiosk_socket(mut socket: WebSocket, mut rx: watch::Receiver<Option<CaseState>>) {
    // Send the current state immediately (or `null` if no active case).
    let current = rx.borrow_and_update().clone();
    if send_state(&mut socket, &current).await.is_err() {
        return;
    }

    // Push on every subsequent change until the client drops or the channel closes.
    loop {
        if rx.changed().await.is_err() {
            debug!("kiosk WS: broadcast closed; dropping connection");
            break;
        }
        let state = rx.borrow_and_update().clone();
        if send_state(&mut socket, &state).await.is_err() {
            debug!("kiosk WS: client gone; dropping connection");
            break;
        }
    }
}

/// Serialise `Option<CaseState>` (the full object, or the literal `null`) and
/// send it as a WS text frame.
async fn send_state(socket: &mut WebSocket, state: &Option<CaseState>) -> Result<(), ()> {
    let json = match serde_json::to_string(state) {
        Ok(j) => j,
        Err(e) => {
            warn!(error = %e, "kiosk WS: failed to serialise CaseState");
            return Ok(()); // skip this update, keep the connection
        }
    };
    socket.send(Message::Text(json)).await.map_err(|_| ())
}

/// `GET /stills/:id` → the JPEG for still `<id>`. We strip a trailing `.jpg` so
/// both `/stills/foo` and `/stills/foo.jpg` work (the HUD references
/// `/stills/<id>.jpg`). Resolution order: the current case's `stills/` first,
/// then a scan of every case dir (a still on screen may belong to a just-cleared
/// case). 404 if nowhere.
async fn still(AxumPath(id): AxumPath<String>, State(state): State<AppState>) -> Response {
    let id = id.strip_suffix(".jpg").unwrap_or(&id).to_string();
    if id.is_empty() || id.contains('/') || id.contains("..") {
        return StatusCode::NOT_FOUND.into_response();
    }

    // 1. Try the current case. Clone the case id out of the watch guard FIRST so
    //    the (non-Send) borrow guard is not held across the await below.
    let current_case_id = state.rx.borrow().as_ref().map(|c| c.case_id.clone());
    if let Some(case_id) = current_case_id {
        let p = state
            .case_base
            .join("cases")
            .join(&case_id)
            .join("stills")
            .join(format!("{id}.jpg"));
        if let Some(resp) = read_jpeg(&p).await {
            return resp;
        }
    }

    // 2. Fall back to scanning every case dir (ids are case-scoped, but a
    //    cleared case can still be on screen).
    let cases_root = state.case_base.join("cases");
    if let Ok(mut entries) = tokio::fs::read_dir(&cases_root).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path().join("stills").join(format!("{id}.jpg"));
            if let Some(resp) = read_jpeg(&p).await {
                return resp;
            }
        }
    }

    StatusCode::NOT_FOUND.into_response()
}

/// Read a JPEG file and wrap it in a `200 image/jpeg` response; `None` if the
/// file is missing/unreadable (the caller maps that to a 404 / next candidate).
async fn read_jpeg(path: &Path) -> Option<Response> {
    let bytes = tokio::fs::read(path).await.ok()?;
    Some(
        (
            [(header::CONTENT_TYPE, "image/jpeg")],
            bytes,
        )
            .into_response(),
    )
}
