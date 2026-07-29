//! End-to-end tests for the stateless Streamable HTTP transport against a
//! minimal in-process server that records exactly what it received.
//!
//! These assert the wire-level obligations of the 2026-07-28 revision, which
//! unit tests over the envelope alone cannot: what actually leaves the socket,
//! and what happens when a response stream dies mid-request.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use chuk_mcp::protocol::json_rpc::{create_request, JsonRpcMessage, RequestId};
use chuk_mcp::protocol::messages::send_message::{ReadStream, WriteStream};
use chuk_mcp::protocol::types::errors::{
    is_local_error, LOCAL_MALFORMED_RESPONSE, LOCAL_REQUEST_REJECTED, LOCAL_STREAM_LOST,
    LOCAL_TRANSPORT_FAILURE,
};
use chuk_mcp::transports::http_modern::{ModernHttpParameters, ModernHttpTransport};
use chuk_mcp::transports::Transport;

/// One request as the server saw it.
#[derive(Debug, Clone)]
struct Seen {
    headers: Vec<(String, String)>,
    body: Value,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn meta(&self) -> &Value {
        &self.body["params"]["_meta"]
    }
    fn id(&self) -> &Value {
        &self.body["id"]
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Behaviour {
    /// Answer every request with a single JSON object.
    Json,
    /// Answer the first request with a stream that closes without a response,
    /// then answer normally. Exercises the re-issue path.
    BreakFirstStream,
    /// Reject with an HTTP status and a JSON-RPC error body — how a modern
    /// server reports `-32020`/`-32021`/`-32022`.
    JsonRpcError(u16, i64),
    /// Reject with a status and a body that is not JSON-RPC at all, as a
    /// gateway or a legacy server would.
    PlainError(u16),
    /// A 200 whose body is not parseable.
    Malformed,
    /// A 200 with no body.
    EmptyOk,
    /// A well-formed SSE stream padded with keep-alive comments and an event
    /// type the client must ignore, then the real response.
    SseWithNoise,
}

type Log = Arc<Mutex<Vec<Seen>>>;

async fn spawn_server(behaviour: Behaviour) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let seen_count = Arc::new(AtomicUsize::new(0));

    let log_for_task = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            let log = log_for_task.clone();
            let counter = seen_count.clone();
            tokio::spawn(handle_conn(socket, behaviour, log, counter));
        }
    });
    (format!("http://{addr}/mcp"), log)
}

async fn handle_conn(
    mut socket: TcpStream,
    behaviour: Behaviour,
    log: Log,
    counter: Arc<AtomicUsize>,
) {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];

    let (head, body) = loop {
        let n = match socket.read(&mut tmp).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&tmp[..n]);

        let Some(end) = find_subslice(&buf, b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buf[..end]).to_string();
        let length = head
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);

        let start = end + 4;
        if buf.len() - start >= length {
            break (head, buf[start..start + length].to_vec());
        }
    };

    let headers: Vec<(String, String)> = head
        .lines()
        .skip(1)
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = parsed.get("id").cloned();

    log.lock().unwrap().push(Seen {
        headers,
        body: parsed,
    });
    let nth = counter.fetch_add(1, Ordering::SeqCst);

    // A notification gets 202 and nothing else.
    let Some(id) = id else {
        let _ = socket
            .write_all(b"HTTP/1.1 202 Accepted\r\nContent-Length: 0\r\n\r\n")
            .await;
        return;
    };

    match behaviour {
        Behaviour::JsonRpcError(status, code) => {
            let body = json!({
                "jsonrpc": "2.0", "id": id,
                "error": {
                    "code": code,
                    "message": "rejected",
                    "data": {"supported": ["2026-07-28"]}
                }
            })
            .to_string();
            let head = format!(
                "HTTP/1.1 {status} Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
            return;
        }
        Behaviour::PlainError(status) => {
            let body = "<html>no MCP endpoint here</html>";
            let head = format!(
                "HTTP/1.1 {status} Not Found\r\nContent-Type: text/html\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
            return;
        }
        Behaviour::Malformed => {
            let body = "{not json at all";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
            return;
        }
        Behaviour::EmptyOk => {
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
            return;
        }
        Behaviour::SseWithNoise => {
            let progress = json!({
                "jsonrpc": "2.0",
                "method": "notifications/progress",
                "params": {"progressToken": "t", "progress": 1}
            });
            let response = json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"resultType": "complete", "ok": true}
            });
            let mut body = String::new();
            // Keep-alive comment: an SSE line starting with ':' carries no data
            // and must be ignored rather than treated as malformed.
            body.push_str(":\n\n");
            body.push_str(&format!("event: message\ndata: {progress}\n\n"));
            // An event type the client has no business acting on.
            body.push_str("event: ping\ndata: {\"ignored\":true}\n\n");
            // A data line that is not JSON.
            body.push_str("event: message\ndata: not-json\n\n");
            body.push_str(&format!("event: message\ndata: {response}\n\n"));

            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(head.as_bytes()).await;
            let _ = socket.write_all(body.as_bytes()).await;
            return;
        }
        Behaviour::Json | Behaviour::BreakFirstStream => {}
    }

    if behaviour == Behaviour::BreakFirstStream && nth == 0 {
        // Open an SSE stream, emit a progress notification, then close the
        // connection without ever sending the response. With no Content-Length
        // the body ends at EOF, which is exactly a lost response stream.
        let progress = json!({
            "jsonrpc": "2.0",
            "method": "notifications/progress",
            "params": {"progressToken": "t", "progress": 1}
        });
        let head =
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
        let _ = socket.write_all(head.as_bytes()).await;
        let _ = socket
            .write_all(format!("event: message\ndata: {progress}\n\n").as_bytes())
            .await;
        let _ = socket.flush().await;
        return; // socket drops -> stream ends with no response
    }

    let response = json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"resultType": "complete", "ok": true}
    })
    .to_string();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        response.len()
    );
    let _ = socket.write_all(head.as_bytes()).await;
    let _ = socket.write_all(response.as_bytes()).await;
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Send one request and wait for the matching response.
async fn call(
    read: &ReadStream,
    write: &WriteStream,
    method: &str,
    params: Value,
    id: RequestId,
) -> JsonRpcMessage {
    write
        .send(JsonRpcMessage::Request(create_request(
            method,
            Some(params),
            Some(id.clone()),
            None,
        )))
        .await
        .unwrap();

    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let mut guard = read.lock().await;
            guard.recv().await
        })
        .await
        .expect("timed out waiting for a response")
        .expect("stream closed");
        if message.id() == Some(&id) {
            return message;
        }
        // A request-scoped notification; keep waiting for the response.
    }
}

#[tokio::test]
async fn sends_the_required_headers_and_meta_and_no_session_id() {
    let (url, log) = spawn_server(Behaviour::Json).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    let response = call(
        &read,
        &write,
        "tools/call",
        json!({"name": "get_weather", "arguments": {"location": "Seattle"}}),
        RequestId::Num(1),
    )
    .await;
    assert!(matches!(response, JsonRpcMessage::Response(_)));

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let request = &seen[0];

    // Mirrored headers.
    assert_eq!(request.header("MCP-Protocol-Version"), Some("2026-07-28"));
    assert_eq!(request.header("Mcp-Method"), Some("tools/call"));
    assert_eq!(request.header("Mcp-Name"), Some("get_weather"));

    // Both content types must be acceptable: the server picks per request.
    let accept = request.header("Accept").unwrap();
    assert!(accept.contains("application/json"), "{accept}");
    assert!(accept.contains("text/event-stream"), "{accept}");

    // The header must agree with the body, or a server returns -32020.
    assert_eq!(
        request.meta()["io.modelcontextprotocol/protocolVersion"],
        json!("2026-07-28")
    );
    assert_eq!(
        request.body["params"]["name"],
        json!("get_weather"),
        "body name must match the Mcp-Name header"
    );

    // Per-request metadata the transport injects, so no caller can omit it.
    assert!(request.meta()["io.modelcontextprotocol/clientCapabilities"].is_object());
    assert_eq!(
        request.meta()["io.modelcontextprotocol/clientInfo"]["name"],
        json!("chuk-mcp-client")
    );

    // The whole point of the revision: no protocol session.
    assert!(
        request.header("Mcp-Session-Id").is_none(),
        "a modern request must never carry a session id"
    );
}

#[tokio::test]
async fn a_broken_response_stream_is_reissued_under_a_new_id() {
    let (url, log) = spawn_server(Behaviour::BreakFirstStream).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    let caller_id = RequestId::Num(7);
    let response = call(&read, &write, "tools/list", json!({}), caller_id.clone()).await;

    // The caller gets its answer under the id *it* used, even though the
    // successful attempt went out under a different one.
    match &response {
        JsonRpcMessage::Response(r) => {
            assert_eq!(r.id, caller_id);
            assert_eq!(r.result["ok"], json!(true));
        }
        other => panic!("expected a response, got {other:?}"),
    }

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "the lost request should have been re-issued");

    // Resumability is gone, so the retry must be a *new* request, not a replay.
    assert_eq!(seen[0].id(), &json!(7));
    assert_ne!(
        seen[1].id(),
        &json!(7),
        "a re-issued request must use a new id, not reuse the lost one"
    );
    assert_ne!(seen[0].id(), seen[1].id());

    // The retry is still a fully-formed modern request.
    assert_eq!(seen[1].header("Mcp-Method"), Some("tools/list"));
    assert_eq!(seen[1].header("MCP-Protocol-Version"), Some("2026-07-28"));
    assert!(seen[1].header("Mcp-Session-Id").is_none());
}

#[tokio::test]
async fn retries_are_bounded_and_then_reported() {
    // Every attempt loses its stream. The caller must get an error rather than
    // hanging or retrying forever.
    let (url, log) = spawn_server(Behaviour::BreakFirstStream).await;
    // Force every attempt to break by allowing no successful path: a fresh
    // server whose first-and-only behaviour breaks, with retries capped at 0.
    let params = ModernHttpParameters::new(&url)
        .unwrap()
        .with_max_stream_retries(0);
    let transport = ModernHttpTransport::start(params).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    let response = call(&read, &write, "tools/list", json!({}), RequestId::Num(9)).await;
    match &response {
        JsonRpcMessage::Error(e) => {
            assert!(
                e.error.message.contains("response stream ended"),
                "unexpected message: {}",
                e.error.message
            );
            // Locally raised, so it must not look like something the peer said.
            assert_eq!(e.error.code, LOCAL_STREAM_LOST);
            assert!(is_local_error(e.error.code));
        }
        other => panic!("expected an error, got {other:?}"),
    }
    // With retries disabled the request is sent exactly once.
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_notification_needs_no_response() {
    let (url, log) = spawn_server(Behaviour::Json).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (_read, write) = transport.get_streams().await.unwrap();

    write
        .send(JsonRpcMessage::Notification(
            chuk_mcp::protocol::json_rpc::create_notification(
                "notifications/progress",
                Some(json!({"progressToken": "t", "progress": 1})),
            ),
        ))
        .await
        .unwrap();

    // Give the dispatcher a moment to POST it.
    for _ in 0..50 {
        if !log.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(
        seen[0].body.get("id").is_none(),
        "notifications carry no id"
    );
    assert_eq!(seen[0].header("Mcp-Method"), Some("notifications/progress"));
    assert!(seen[0].header("Mcp-Session-Id").is_none());
}

#[tokio::test]
async fn a_modern_protocol_error_reaches_the_caller_intact() {
    // This is how a client learns it asked for an unsupported version. If the
    // transport flattened it into a generic "HTTP 400", `renegotiate` would
    // have nothing to work with and a recoverable situation would look fatal.
    use chuk_mcp::protocol::types::errors::UNSUPPORTED_PROTOCOL_VERSION;

    let (url, _log) =
        spawn_server(Behaviour::JsonRpcError(400, UNSUPPORTED_PROTOCOL_VERSION)).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    let response = call(&read, &write, "tools/list", json!({}), RequestId::Num(1)).await;
    match &response {
        JsonRpcMessage::Error(e) => {
            assert_eq!(e.error.code, UNSUPPORTED_PROTOCOL_VERSION);
            // The peer's own code, passed through — emphatically *not* local.
            assert!(!is_local_error(e.error.code));
            // The `supported` list survives, so the client can retry.
            assert_eq!(
                e.error.data.as_ref().unwrap()["supported"],
                json!(["2026-07-28"])
            );
        }
        other => panic!("expected the server's own JSON-RPC error, got {other:?}"),
    }
}

#[tokio::test]
async fn a_non_jsonrpc_error_body_still_reaches_the_caller() {
    // A bare 404 from something that is not an MCP endpoint. The caller must
    // get an error rather than hang; era fallback is decided a layer up.
    let (url, _log) = spawn_server(Behaviour::PlainError(404)).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    let response = call(&read, &write, "tools/list", json!({}), RequestId::Num(1)).await;
    match &response {
        JsonRpcMessage::Error(e) => {
            assert!(e.error.message.contains("404"), "{}", e.error.message);
            // Our summary of an HTTP failure, not a JSON-RPC error the peer sent.
            assert_eq!(e.error.code, LOCAL_TRANSPORT_FAILURE);
        }
        other => panic!("expected an error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_unparseable_body_becomes_a_parse_error() {
    let (url, _log) = spawn_server(Behaviour::Malformed).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    let response = call(&read, &write, "tools/list", json!({}), RequestId::Num(1)).await;
    match &response {
        JsonRpcMessage::Error(e) => {
            assert!(
                e.error.message.contains("Parse error"),
                "{}",
                e.error.message
            );
            assert_eq!(e.error.code, LOCAL_MALFORMED_RESPONSE);
        }
        other => panic!("expected a parse error, got {other:?}"),
    }
}

#[tokio::test]
async fn an_empty_body_does_not_hang_the_caller() {
    let (url, _log) = spawn_server(Behaviour::EmptyOk).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    // A request answered with no body would otherwise block forever.
    let response = call(&read, &write, "tools/list", json!({}), RequestId::Num(1)).await;
    assert!(matches!(response, JsonRpcMessage::Response(_)));
}

#[tokio::test]
async fn sse_noise_is_ignored_and_the_response_still_arrives() {
    // Keep-alive comments, foreign event types and non-JSON data lines all
    // appear on real streams; none may derail the response.
    let (url, _log) = spawn_server(Behaviour::SseWithNoise).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    let response = call(&read, &write, "tools/list", json!({}), RequestId::Num(4)).await;
    match &response {
        JsonRpcMessage::Response(r) => {
            assert_eq!(r.id, RequestId::Num(4));
            assert_eq!(r.result["ok"], json!(true));
        }
        other => panic!("expected a response, got {other:?}"),
    }
}

#[tokio::test]
async fn a_batch_cannot_be_sent_on_the_modern_transport() {
    // One request or notification per POST: a batch has no single method to
    // mirror into Mcp-Method, so it is refused locally.
    let (url, log) = spawn_server(Behaviour::Json).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    write
        .send(JsonRpcMessage::BatchRequest(vec![JsonRpcMessage::Request(
            create_request("tools/list", None, Some(RequestId::Num(1)), None),
        )]))
        .await
        .unwrap();

    // A batch has no id, so nothing is routed back; assert it never went out.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert!(log.lock().unwrap().is_empty());
    drop(read);
}

#[tokio::test]
async fn closing_the_transport_stops_its_task() {
    let (url, _log) = spawn_server(Behaviour::Json).await;
    let mut transport =
        ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    transport.close().await.unwrap();
    // Idempotent.
    transport.close().await.unwrap();
}

#[tokio::test]
async fn a_missing_mcp_name_source_fails_without_a_round_trip() {
    let (url, log) = spawn_server(Behaviour::Json).await;
    let transport = ModernHttpTransport::start(ModernHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    // tools/call requires Mcp-Name, sourced from params.name.
    let response = call(&read, &write, "tools/call", json!({}), RequestId::Num(3)).await;
    match &response {
        JsonRpcMessage::Error(e) => assert_eq!(e.error.code, LOCAL_REQUEST_REJECTED),
        other => panic!("expected a local rejection, got {other:?}"),
    }
    assert!(
        log.lock().unwrap().is_empty(),
        "the request should never have reached the server"
    );
}
