//! Home Assistant clients: a WebSocket event subscriber (`ws`) and a REST
//! camera-still fetcher (`rest`).

pub mod rest;
pub mod ws;

pub use rest::RestClient;
pub use ws::HaEvent;

/// Convert an HA base URL (`http://host:8123`) to its WebSocket endpoint
/// (`ws://host:8123/api/websocket`).
pub fn websocket_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let scheme_swapped = if let Some(rest) = base.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = base.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        base.to_string()
    };
    format!("{scheme_swapped}/api/websocket")
}
