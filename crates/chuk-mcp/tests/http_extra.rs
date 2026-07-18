//! Extended Streamable HTTP transport coverage: JSON + SSE responses, error
//! status, empty-body, session-id capture, notifications, and parameters.

use std::collections::HashMap;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use chuk_mcp::client::McpClient;
use chuk_mcp::transports::http::{StreamableHttpParameters, StreamableHttpTransport};

async fn spawn_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(handle_conn(socket));
        }
    });
    format!("http://{addr}/mcp")
}

async fn handle_conn(mut socket: TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    let body = loop {
        let n = match socket.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buf[..end]).to_lowercase();
        let len = headers
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let start = end + 4;
        if buf.len() - start >= len {
            break buf[start..start + len].to_vec();
        }
    };
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(id) = req.get("id").cloned() else {
        // notification
        let _ = socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    };
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let tool = req["params"]["name"].as_str().unwrap_or("");

    match (method, tool) {
        ("initialize", _) => {
            let body = json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": req["params"]["protocolVersion"],
                "serverInfo": {"name": "http-x", "version": "1"},
                "capabilities": {"tools": {}},
            }})
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: sess-1\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        ("ping", _) => {
            // empty body -> transport synthesizes an empty success result
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
        ("tools/call", "sse_tool") => {
            let payload = json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "sse-ok"}]}});
            let sse = format!("event: message\ndata: {payload}\n\n");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(), sse
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        ("tools/call", "err_tool") => {
            let _ = socket
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom")
                .await;
        }
        ("tools/call", "sse_body") => {
            // An SSE-formatted body served with a JSON content type -> the
            // transport's parse_sse_text fallback handles it.
            let payload = json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "sse-body-ok"}]}});
            let sse = format!("event: message\ndata: {payload}\n\n");
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(), sse
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        ("tools/call", "crlf_tool") => {
            // SSE with CRLF separators + a non-message event (skipped).
            let payload = json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "crlf-ok"}]}});
            let sse = format!(
                "event: notice\r\ndata: ignore\r\n\r\nevent: message\r\ndata: {payload}\r\n\r\n"
            );
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                sse.len(), sse
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        ("tools/call", "bad_body") => {
            // Valid JSON but not a valid JSON-RPC message -> parse error path.
            let body = "{\"foo\":1}";
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
        _ => {
            let body = json!({"jsonrpc": "2.0", "id": id, "result": {}}).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(), body
            );
            let _ = socket.write_all(resp.as_bytes()).await;
        }
    }
}

#[tokio::test]
async fn http_transport_scenarios() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_test_writer()
        .try_init();
    let url = spawn_server().await;
    let params = StreamableHttpParameters::new(&url)
        .unwrap()
        .with_bearer_token("secret");
    let transport = StreamableHttpTransport::start(params).unwrap();
    let mut client = McpClient::new(transport);

    // initialize: JSON response + Mcp-Session-Id capture + initialized notification (202)
    let init = client.initialize().await.unwrap();
    assert_eq!(init.server_info.name, "http-x");

    // empty-body path
    assert!(client.ping().await);

    // SSE response path
    let result = client.call_tool("sse_tool", json!({})).await.unwrap();
    assert_eq!(result.text(), "sse-ok");

    // SSE-formatted body under a JSON content type (parse_sse_text fallback)
    let result = client.call_tool("sse_body", json!({})).await.unwrap();
    assert_eq!(result.text(), "sse-body-ok");

    // CRLF SSE + a non-message event that is skipped
    let result = client.call_tool("crlf_tool", json!({})).await.unwrap();
    assert_eq!(result.text(), "crlf-ok");

    // error-status path -> retryable error
    let err = client.call_tool("err_tool", json!({})).await.unwrap_err();
    assert!(err.is_retryable());

    // valid-JSON-but-invalid-JSON-RPC body -> parse error routed to the caller
    let err = client.call_tool("bad_body", json!({})).await.unwrap_err();
    assert!(err.code().is_some());

    client.close().await.unwrap();
}

#[test]
fn parameter_builders() {
    assert!(StreamableHttpParameters::new("").is_err());
    assert!(StreamableHttpParameters::new("ftp://x").is_err());

    let p = StreamableHttpParameters::new("http://host/mcp/")
        .unwrap()
        .with_bearer_token("Bearer abc");
    assert_eq!(p.url, "http://host/mcp");
    assert_eq!(p.timeout, 60.0);
    assert!(p.session_id.is_none());
    assert!(p.enable_streaming);

    let mut headers = HashMap::new();
    headers.insert("X-Custom".to_string(), "1".to_string());
    let p = StreamableHttpParameters::new("https://host")
        .unwrap()
        .with_headers(headers);
    assert_eq!(p.url, "https://host");
}
