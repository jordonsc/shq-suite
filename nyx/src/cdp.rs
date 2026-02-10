use anyhow::{bail, Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, tungstenite::Message};

const CDP_ADDR: &str = "127.0.0.1:9222";

#[derive(Debug, Deserialize)]
struct CdpTarget {
    #[serde(rename = "type")]
    target_type: String,
    #[serde(rename = "webSocketDebuggerUrl")]
    web_socket_debugger_url: Option<String>,
    url: String,
}

/// Navigate Chrome to a new URL via the Chrome DevTools Protocol.
pub async fn navigate(url: &str) -> Result<()> {
    let target = discover_page_target().await?;

    let ws_url = target
        .web_socket_debugger_url
        .context("Page target has no WebSocket debugger URL")?;

    tracing::debug!("Connecting to CDP WebSocket: {}", ws_url);

    let (mut ws, _) = connect_async(&ws_url)
        .await
        .with_context(|| format!("Failed to connect to Chrome CDP WebSocket at {}", ws_url))?;

    let cmd = serde_json::json!({
        "id": 1,
        "method": "Page.navigate",
        "params": { "url": url }
    });

    ws.send(Message::Text(cmd.to_string()))
        .await
        .context("Failed to send navigate command")?;

    // Wait for the response
    if let Some(Ok(Message::Text(response))) = ws.next().await {
        let resp: serde_json::Value = serde_json::from_str(&response)?;
        if let Some(error) = resp.get("error") {
            bail!("CDP navigate error: {}", error);
        }
    }

    ws.close(None).await.ok();
    Ok(())
}

/// Get the current URL of the Chrome page target.
pub async fn get_current_url() -> Result<String> {
    let target = discover_page_target().await?;
    Ok(target.url)
}

/// Discover the first page-type target from Chrome's debug endpoint.
fn discover_page_target() -> impl std::future::Future<Output = Result<CdpTarget>> {
    async {
        let body = http_get_targets().await?;

        let targets: Vec<CdpTarget> =
            serde_json::from_str(&body).context("Failed to parse CDP targets JSON")?;

        targets
            .into_iter()
            .find(|t| t.target_type == "page")
            .context("No page target found — is Chrome running?")
    }
}

/// Raw HTTP GET to Chrome's /json endpoint. No extra dependencies needed
/// since we're only ever talking to localhost.
async fn http_get_targets() -> Result<String> {
    let stream = TcpStream::connect(CDP_ADDR).await.context(
        "Failed to connect to Chrome remote debugging port. \
         Is Chrome running with --remote-debugging-port=9222?",
    )?;

    let (read_half, mut write_half) = tokio::io::split(stream);

    write_half
        .write_all(b"GET /json HTTP/1.1\r\nHost: 127.0.0.1:9222\r\nConnection: close\r\n\r\n")
        .await?;

    let mut reader = BufReader::new(read_half);
    let mut content_length: Option<usize> = None;

    // Read headers line-by-line until the blank line.
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line == "\r\n" || line.is_empty() {
            break;
        }
        if line.to_ascii_lowercase().starts_with("content-length:") {
            content_length = line.split(':').nth(1).and_then(|v| v.trim().parse().ok());
        }
    }

    // Read exactly Content-Length bytes so we don't block waiting for EOF.
    let body = match content_length {
        Some(len) => {
            let mut buf = vec![0u8; len];
            reader.read_exact(&mut buf).await?;
            buf
        }
        None => {
            // Fallback: read to end with a timeout.
            let mut buf = Vec::new();
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                reader.read_to_end(&mut buf),
            )
            .await
            .context("Timeout reading from Chrome debug port")??;
            buf
        }
    };

    String::from_utf8(body).context("Non-UTF8 response from Chrome debug port")
}
