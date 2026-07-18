//! End-to-end test: the Streamable HTTP client transport against a minimal
//! in-process HTTP MCP server. Exercises both immediate-JSON responses and
//! SSE-formatted responses.

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use chuk_mcp::client::McpClient;
use chuk_mcp::transports::http::{StreamableHttpParameters, StreamableHttpTransport};

/// Spawn the test server; returns its base URL.
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

/// Handle one HTTP request/response on a connection, then close it.
async fn handle_conn(mut socket: TcpStream) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];

    // Read until headers complete, then the full Content-Length body.
    let body = loop {
        let n = match socket.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);

        let Some(header_end) = find_subslice(&buf, b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
        let content_length = headers
            .lines()
            .find_map(|l| l.strip_prefix("content-length:"))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);

        let body_start = header_end + 4;
        if buf.len() - body_start >= content_length {
            break buf[body_start..body_start + content_length].to_vec();
        }
    };

    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str).unwrap_or("");

    // Notifications (no id) get a 202 with no body.
    let Some(id) = id else {
        let _ = socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await;
        return;
    };

    let result = match method {
        "initialize" => json!({
            "protocolVersion": request["params"]["protocolVersion"],
            "serverInfo": {"name": "http-test-server", "version": "9.9"},
            "capabilities": {"tools": {}},
        }),
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": [
                {"name": "echo", "description": "echo text",
                 "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}},
            ]
        }),
        "tools/call" => {
            let text = request["params"]["arguments"]["text"]
                .as_str()
                .unwrap_or("")
                .to_string();
            json!({"content": [{"type": "text", "text": format!("echo: {text}")}]})
        }
        _ => {
            write_error(&mut socket, &id, -32601, "method not found").await;
            return;
        }
    };

    let response = json!({"jsonrpc": "2.0", "id": id, "result": result});

    // Deliver tools/call via SSE, everything else as immediate JSON — so the
    // test exercises both response paths in the transport.
    if method == "tools/call" {
        write_sse(&mut socket, &response).await;
    } else {
        write_json(&mut socket, &response).await;
    }
}

async fn write_json(socket: &mut TcpStream, value: &Value) {
    let body = value.to_string();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;
}

async fn write_sse(socket: &mut TcpStream, value: &Value) {
    let body = format!("event: message\ndata: {}\n\n", value);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = socket.write_all(response.as_bytes()).await;
}

async fn write_error(socket: &mut TcpStream, id: &Value, code: i64, message: &str) {
    let value = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    write_json(socket, &value).await;
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[tokio::test]
async fn streamable_http_roundtrip() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_test_writer()
        .try_init();
    let url = spawn_server().await;

    let params = StreamableHttpParameters::new(url).unwrap();
    let transport = StreamableHttpTransport::start(params).unwrap();
    let mut client = McpClient::new(transport);

    let init = client.initialize().await.expect("initialize");
    assert_eq!(init.server_info.name, "http-test-server");

    assert!(client.ping().await);

    let tools = client.list_tools().await.expect("list tools");
    assert_eq!(tools[0].name, "echo");

    // This response comes back over SSE.
    let result = client
        .call_tool("echo", json!({"text": "hi there"}))
        .await
        .expect("call echo");
    assert_eq!(result.text(), "echo: hi there");

    client.close().await.expect("close");
}
