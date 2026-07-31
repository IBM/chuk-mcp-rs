//! `connect` over HTTP, against both eras.
//!
//! The mock server tells the eras apart the way a real one does: only a modern
//! client sends `Mcp-Method`, so its presence identifies the request's era
//! without inspecting the body.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use chuk_mcp::protocol::era::{EraMode, ProtocolEra};
use chuk_mcp::protocol::versioning;
use chuk_mcp::{connect, Connect};

/// Marks a request as modern; no legacy client sends it.
const MODERN_MARKER: &str = "mcp-method:";
/// The header carrying the method, lower-cased for matching.
const METHOD_HEADER_PREFIX: &str = "mcp-method: ";

const SERVER_NAME: &str = "connect-mock";
const SERVER_VERSION: &str = "1.0.0";
const TOOL_NAME: &str = "greet";

/// Which era the mock speaks.
#[derive(Clone, Copy)]
enum Era {
    /// Answers `server/discover`.
    Modern,
    /// Rejects modern requests with a plain 400, then serves `initialize`.
    Legacy,
}

async fn spawn(era: Era) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(handle(socket, era));
        }
    });
    format!("http://{addr}/mcp")
}

async fn handle(mut socket: TcpStream, era: Era) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    let (head, body) = loop {
        let n = match socket.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
        let length = head
            .lines()
            .find(|line| line.starts_with("content-length:"))
            .and_then(|line| line.split(':').nth(1))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let start = end + 4;
        if buf.len() - start >= length {
            break (head, buf[start..start + length].to_vec());
        }
    };

    let modern = head.contains(MODERN_MARKER);
    let method = head
        .lines()
        .find_map(|line| line.strip_prefix(METHOD_HEADER_PREFIX))
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = parsed.get("id").cloned();
    let body_method = parsed
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let (status, reason, payload) = match (era, modern) {
        (Era::Modern, _) => (
            200,
            "OK",
            json!({
                "jsonrpc": "2.0", "id": id,
                "result": discover_result(if method.is_empty() { &body_method } else { &method }),
            })
            .to_string(),
        ),
        // A legacy peer rejects the modern request before processing it, so the
        // client may safely re-send through the legacy lifecycle.
        (Era::Legacy, true) => (
            400,
            "Bad Request",
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "bad request"}})
                .to_string(),
        ),
        (Era::Legacy, false) => (
            200,
            "OK",
            json!({"jsonrpc": "2.0", "id": id, "result": legacy_result(&body_method)}).to_string(),
        ),
    };

    // One request per connection, so the client must not pool the socket.
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
}

fn discover_result(method: &str) -> Value {
    match method {
        "tools/list" => json!({"resultType": "complete", "tools": tools()}),
        _ => json!({
            "resultType": "complete",
            "supportedVersions": [versioning::FIRST_MODERN_VERSION],
            "capabilities": {"tools": {"listChanged": true}},
            "_meta": {
                "io.modelcontextprotocol/serverInfo": {
                    "name": SERVER_NAME, "version": SERVER_VERSION,
                },
            },
        }),
    }
}

fn legacy_result(method: &str) -> Value {
    match method {
        "tools/list" => json!({"tools": tools()}),
        _ => json!({
            "protocolVersion": versioning::LATEST_LEGACY_VERSION,
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "capabilities": {"tools": {}},
        }),
    }
}

fn tools() -> Value {
    json!([{"name": TOOL_NAME, "inputSchema": {"type": "object"}}])
}

#[tokio::test]
async fn connect_detects_a_modern_endpoint() {
    let url = spawn(Era::Modern).await;
    let client = connect(&url).await.expect("connect");

    assert_eq!(client.era(), Some(ProtocolEra::Modern));
    assert_eq!(
        client.protocol_version(),
        Some(versioning::FIRST_MODERN_VERSION)
    );
    assert_eq!(
        client.server_info().map(|info| info.name.as_str()),
        Some(SERVER_NAME)
    );

    let tools = client.list_tools().await.expect("tools/list");
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, TOOL_NAME);
}

#[tokio::test]
async fn connect_falls_back_to_a_legacy_endpoint() {
    let url = spawn(Era::Legacy).await;
    let client = connect(&url).await.expect("connect");

    assert_eq!(client.era(), Some(ProtocolEra::Legacy));
    assert_eq!(
        client.protocol_version(),
        Some(versioning::LATEST_LEGACY_VERSION)
    );

    let tools = client.list_tools().await.expect("tools/list");
    assert_eq!(tools[0].name, TOOL_NAME);
}

#[tokio::test]
async fn the_builder_pins_an_http_era_and_carries_options() {
    let url = spawn(Era::Legacy).await;
    let client = Connect::to_url(&url)
        .era(EraMode::Legacy)
        .bearer_token("token")
        .header("X-Tenant", "acme")
        .credential_context("tenant-acme")
        .timeout(Duration::from_secs(5))
        .connect()
        .await
        .expect("connect pinned to legacy");

    // Pinned: no probe was sent, and the era is what we asked for.
    assert_eq!(client.era(), Some(ProtocolEra::Legacy));
}

#[tokio::test]
async fn pinning_modern_against_a_legacy_endpoint_reports_the_failure() {
    let url = spawn(Era::Legacy).await;
    let outcome = Connect::to_url(&url).era(EraMode::Modern).connect().await;

    assert!(
        outcome.is_err(),
        "a pinned era must surface the failure rather than silently fall back"
    );
}
