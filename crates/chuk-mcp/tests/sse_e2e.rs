//! End-to-end coverage for the legacy SSE transport: connect via the endpoint
//! event, immediate-200 responses, async 202 + SSE-delivered responses, an
//! error status, and the connection accessors.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use chuk_mcp::client::McpClient;
use chuk_mcp::transports::sse::{SseParameters, SseTransport};

/// Channel the POST handlers use to push SSE events onto the live GET stream.
type Sink = Arc<Mutex<Option<mpsc::Sender<String>>>>;

async fn spawn_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let sink: Sink = Arc::new(Mutex::new(None));
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let sink = sink.clone();
            tokio::spawn(handle_conn(socket, sink));
        }
    });
    format!("http://{addr}")
}

async fn read_head(socket: &mut TcpStream) -> (String, Vec<u8>) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let len = head
                .to_lowercase()
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().to_string())
                })
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = buf[end + 4..].to_vec();
            while body.len() < len {
                let n = socket.read(&mut tmp).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&tmp[..n]);
            }
            return (head, body);
        }
        let n = match socket.read(&mut tmp).await {
            Ok(0) | Err(_) => return (String::new(), Vec::new()),
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);
    }
}

async fn handle_conn(mut socket: TcpStream, sink: Sink) {
    let (head, body) = read_head(&mut socket).await;
    let Some(request_line) = head.lines().next() else {
        return;
    };

    if request_line.starts_with("GET") {
        // Open the SSE stream: announce the message endpoint, then forward any
        // pushed events (used for 202 async delivery) plus keepalives.
        let _ = socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: keep-alive\r\n\r\n")
            .await;
        let _ = socket
            .write_all(b"event: endpoint\ndata: /messages/?session_id=abc123\n\n")
            .await;
        let _ = socket.write_all(b"event: keepalive\ndata: k\n\n").await;

        let (tx, mut rx) = mpsc::channel::<String>(16);
        *sink.lock().unwrap() = Some(tx);
        loop {
            tokio::select! {
                event = rx.recv() => match event {
                    Some(e) => { if socket.write_all(e.as_bytes()).await.is_err() { break } }
                    None => break,
                },
                _ = tokio::time::sleep(Duration::from_millis(100)) => {
                    if socket.write_all(b"event: keepalive\ndata: k\n\n").await.is_err() { break }
                }
            }
        }
        return;
    }

    // POST to the message endpoint.
    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let Some(id) = req.get("id").cloned() else {
        let _ = socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    };
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let tool = req["params"]["name"].as_str().unwrap_or("");

    match (method, tool) {
        ("initialize", _) => {
            respond_json(
                &mut socket,
                json!({"jsonrpc": "2.0", "id": id, "result": {
                    "protocolVersion": req["params"]["protocolVersion"],
                    "serverInfo": {"name": "sse-x", "version": "1"},
                    "capabilities": {"tools": {}},
                }}),
            )
            .await;
        }
        ("ping", _) => respond_json(&mut socket, json!({"jsonrpc": "2.0", "id": id, "result": {}})).await,
        ("tools/call", "async_tool") => {
            // Accept, then deliver the response over the SSE stream.
            let _ = socket
                .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
            let payload = json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "async-ok"}]}});
            let event = format!("event: message\ndata: {payload}\n\n");
            let tx = sink.lock().unwrap().clone();
            if let Some(tx) = tx {
                let _ = tx.send(event).await;
            }
        }
        ("tools/call", "err_tool") => {
            let _ = socket
                .write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom")
                .await;
        }
        _ => {
            respond_json(&mut socket, json!({"jsonrpc": "2.0", "id": id, "result": {"content": [{"type": "text", "text": "ok"}]}})).await
        }
    }
}

async fn respond_json(socket: &mut TcpStream, value: Value) {
    let body = value.to_string();
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(resp.as_bytes()).await;
}

#[tokio::test]
async fn sse_transport_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_test_writer()
        .try_init();
    let base = spawn_server().await;

    let params = SseParameters::new(&base).unwrap();
    let transport = SseTransport::start(params).await.unwrap();
    assert!(transport.is_connected());
    assert_eq!(transport.get_session_id().as_deref(), Some("abc123"));

    let mut client = McpClient::new(transport);
    let init = client.initialize().await.unwrap();
    assert_eq!(init.server_info.name, "sse-x");

    assert!(client.ping().await);

    // 202 + SSE-delivered response
    let result = client.call_tool("async_tool", json!({})).await.unwrap();
    assert_eq!(result.text(), "async-ok");

    // error status
    let err = client.call_tool("err_tool", json!({})).await.unwrap_err();
    assert!(err.is_retryable());

    client.close().await.unwrap();
}

#[tokio::test]
async fn sse_with_bearer_token() {
    let base = spawn_server().await;
    let mut params = SseParameters::new(&base).unwrap();
    params.bearer_token = Some("tok".to_string());
    let transport = SseTransport::start(params).await.unwrap();
    assert!(transport.is_connected());
    let mut client = McpClient::new(transport);
    let init = client.initialize().await.unwrap();
    assert_eq!(init.server_info.name, "sse-x");
    client.close().await.unwrap();
}

#[tokio::test]
async fn sse_with_prefixed_bearer_token() {
    let base = spawn_server().await;
    let mut params = SseParameters::new(&base).unwrap();
    params.bearer_token = Some("Bearer already-prefixed".to_string());
    let transport = SseTransport::start(params).await.unwrap();
    assert!(transport.is_connected());
    let mut client = McpClient::new(transport);
    client.initialize().await.unwrap();
    client.close().await.unwrap();
}

#[tokio::test]
async fn sse_connection_failure_times_out() {
    // No server here -> the SSE GET fails to connect, the endpoint event never
    // arrives, and start() times out with an error.
    let mut params = SseParameters::new("http://127.0.0.1:1").unwrap();
    params.timeout = 0.5;
    assert!(SseTransport::start(params).await.is_err());
}

/// A server whose /sse stream announces the endpoint, then pushes an untyped
/// JSON-RPC message, an unknown data line, and a malformed message event —
/// exercising the transport's data-handling branches. When `typed` is false the
/// endpoint is announced without an `event:` line (untyped path).
async fn spawn_endpoint_server(endpoint: String, typed: bool) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let endpoint = Arc::new(endpoint.replace("{PORT}", &addr.port().to_string()));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let endpoint = endpoint.clone();
            tokio::spawn(async move {
                let (head, body) = read_head(&mut socket).await;
                if head.starts_with("GET") {
                    let _ = socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\n")
                        .await;
                    let announce = if typed {
                        format!("event: endpoint\ndata: {endpoint}\n\n")
                    } else {
                        format!("data: {endpoint}\n\n")
                    };
                    let _ = socket.write_all(announce.as_bytes()).await;
                    // untyped JSON-RPC notification -> routed to incoming
                    let _ = socket
                        .write_all(
                            b"data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/x\"}\n\n",
                        )
                        .await;
                    let _ = socket.write_all(b"data: just some text\n\n").await; // unknown -> ignored
                    let _ = socket.write_all(b"event: message\ndata: notjson\n\n").await; // parse error
                    loop {
                        if socket
                            .write_all(b"event: keepalive\ndata: k\n\n")
                            .await
                            .is_err()
                        {
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                } else {
                    let req: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                    if let Some(id) = req.get("id").cloned() {
                        respond_json(
                            &mut socket,
                            json!({"jsonrpc": "2.0", "id": id, "result": {
                                "protocolVersion": req["params"]["protocolVersion"],
                                "serverInfo": {"name": "ep", "version": "1"},
                                "capabilities": {},
                            }}),
                        )
                        .await;
                    } else {
                        let _ = socket
                            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    }
                }
            });
        }
    });
    format!("http://{addr}")
}

async fn connect_endpoint(endpoint: &str, typed: bool) -> SseTransport {
    let base = spawn_endpoint_server(endpoint.to_string(), typed).await;
    let mut params = SseParameters::new(&base).unwrap();
    params.timeout = 4.0;
    SseTransport::start(params).await.unwrap()
}

#[tokio::test]
async fn sse_untyped_endpoint_and_data() {
    // leading-slash endpoint announced as untyped data + bad message events.
    let transport = connect_endpoint("/messages/", false).await;
    assert!(transport.is_connected());
    assert!(transport.get_session_id().is_none());
    let mut client = McpClient::new(transport);
    assert_eq!(client.initialize().await.unwrap().server_info.name, "ep");
    client.close().await.unwrap();
}

#[tokio::test]
async fn sse_endpoint_format_variants() {
    // full-URL form
    let t = connect_endpoint("http://127.0.0.1:{PORT}/mcp", true).await;
    assert!(t.is_connected());
    // query-only form (no leading slash, contains '=')
    let t = connect_endpoint("session_id=q123", true).await;
    assert!(t.is_connected());
    assert_eq!(t.get_session_id().as_deref(), Some("q123"));
}

#[tokio::test]
async fn sse_bareword_endpoint_send_error() {
    // A bareword endpoint becomes an invalid POST URL, so sending a request
    // surfaces a transport error to the caller.
    let t = connect_endpoint("not-a-valid-url", true).await;
    assert!(t.is_connected());
    let mut client = McpClient::new(t);
    assert!(client.initialize().await.is_err());
    client.close().await.unwrap();
}

#[tokio::test]
async fn sse_get_non_200_fails() {
    // The /sse endpoint returns 404 -> connection never establishes.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        if let Ok((mut socket, _)) = listener.accept().await {
            let _ = read_head(&mut socket).await;
            let _ = socket
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .await;
        }
    });
    let mut params = SseParameters::new(format!("http://{addr}")).unwrap();
    params.timeout = 0.5;
    assert!(SseTransport::start(params).await.is_err());
}

#[test]
fn sse_parameter_validation() {
    assert!(SseParameters::new("").is_err());
    assert!(SseParameters::new("ws://x").is_err());
    let p = SseParameters::new("http://host/").unwrap();
    assert_eq!(p.url, "http://host");
    assert_eq!(p.sse_endpoint, "/sse");
    assert_eq!(p.timeout, 60.0);
}
