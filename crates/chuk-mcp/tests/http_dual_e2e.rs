//! End-to-end tests for the dual-era Streamable HTTP transport.
//!
//! The server distinguishes eras the way a real one does: a modern request is
//! recognisable by its `Mcp-Method` header, which no legacy client sends.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use chuk_mcp::protocol::era::{EraMode, ProtocolEra};
use chuk_mcp::protocol::json_rpc::{create_request, JsonRpcMessage, RequestId};
use chuk_mcp::protocol::messages::send_message::{ReadStream, WriteStream};
use chuk_mcp::protocol::types::errors::UNSUPPORTED_PROTOCOL_VERSION;
use chuk_mcp::transports::http_dual::{DualEraHttpParameters, DualEraHttpTransport};
use chuk_mcp::transports::Transport;

#[derive(Debug, Clone)]
struct Seen {
    modern: bool,
    body: Value,
}

type Log = Arc<Mutex<Vec<Seen>>>;

#[derive(Clone, Copy)]
enum Server {
    /// Answers modern requests happily.
    Modern,
    /// Rejects modern requests with a plain JSON-RPC error and a 400, exactly as
    /// a legacy server would when it cannot make sense of the request.
    Legacy,
    /// Has no modern endpoint at all: a bare 404 with an HTML body.
    NoModernEndpoint,
    /// Requires credentials. Proves nothing about the era.
    Unauthorised,
    /// Modern, but rejects the version it was offered.
    WrongVersion,
}

async fn spawn(server: Server) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let log_for_task = log.clone();

    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(handle(socket, server, log_for_task.clone()));
        }
    });
    (format!("http://{addr}/mcp"), log)
}

async fn handle(mut socket: TcpStream, server: Server, log: Log) {
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
            .find(|l| l.starts_with("content-length:"))
            .and_then(|l| l.split(':').nth(1))
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let start = end + 4;
        if buf.len() - start >= length {
            break (head, buf[start..start + length].to_vec());
        }
    };

    // Only a modern client sends Mcp-Method.
    let modern = head.contains("mcp-method:");
    let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let id = parsed.get("id").cloned();
    log.lock().unwrap().push(Seen {
        modern,
        body: parsed,
    });

    let (status, reason, content_type, payload) = match (server, modern) {
        (Server::Modern, _) | (Server::Legacy, false) | (Server::NoModernEndpoint, false) => (
            200,
            "OK",
            "application/json",
            json!({"jsonrpc": "2.0", "id": id, "result": {"resultType": "complete", "ok": true}})
                .to_string(),
        ),
        (Server::Legacy, true) => (
            400,
            "Bad Request",
            "application/json",
            // A JSON-RPC error, but not a *modern* one — so it identifies a
            // legacy peer rather than a modern rejection.
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32600, "message": "bad request"}})
                .to_string(),
        ),
        (Server::NoModernEndpoint, true) => {
            (404, "Not Found", "text/html", "<html>nope</html>".to_string())
        }
        (Server::Unauthorised, _) => (
            401,
            "Unauthorized",
            "text/plain",
            "credentials required".to_string(),
        ),
        (Server::WrongVersion, _) => (
            400,
            "Bad Request",
            "application/json",
            json!({
                "jsonrpc": "2.0", "id": id,
                "error": {
                    "code": UNSUPPORTED_PROTOCOL_VERSION,
                    "message": "Unsupported protocol version",
                    "data": {"supported": ["2026-07-28"]}
                }
            })
            .to_string(),
        ),
    };

    // This server handles one request per connection and then closes, so it
    // must say so — otherwise the client pools the connection and the fallback
    // POST lands on a socket the server has already dropped.
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
}

async fn call(read: &ReadStream, write: &WriteStream, id: RequestId) -> JsonRpcMessage {
    write
        .send(JsonRpcMessage::Request(create_request(
            "tools/list",
            Some(json!({})),
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
        .expect("timed out")
        .expect("closed");
        if message.id() == Some(&id) {
            return message;
        }
    }
}

#[tokio::test]
async fn a_modern_server_is_detected_and_cached() {
    let (url, log) = spawn(Server::Modern).await;
    let transport = DualEraHttpTransport::start(DualEraHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    // Era is unknown until the first response: HTTP has no cheaper probe.
    assert_eq!(transport.era(), None);

    assert!(matches!(
        call(&read, &write, RequestId::Num(1)).await,
        JsonRpcMessage::Response(_)
    ));
    assert_eq!(transport.era(), Some(ProtocolEra::Modern));

    // No wasted probe: the first real call was the probe.
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].modern);
}

#[tokio::test]
async fn a_legacy_server_falls_back_and_the_request_is_resent() {
    let (url, log) = spawn(Server::Legacy).await;
    let transport = DualEraHttpTransport::start(DualEraHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    // The caller sees a success, not the 400 the modern attempt earned.
    match call(&read, &write, RequestId::Num(1)).await {
        JsonRpcMessage::Response(r) => assert_eq!(r.result["ok"], json!(true)),
        other => panic!("expected a successful response, got {other:?}"),
    }
    assert_eq!(transport.era(), Some(ProtocolEra::Legacy));

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 2, "the request should have been re-sent");
    assert!(seen[0].modern, "first attempt is modern — it is the probe");
    assert!(!seen[1].modern, "the retry must use the legacy shape");

    // Same logical call both times: the fallback re-sends the caller's request,
    // not some rewritten version of it.
    assert_eq!(seen[0].body["method"], json!("tools/list"));
    assert_eq!(seen[1].body["method"], json!("tools/list"));
    assert_eq!(seen[1].body["id"], json!(1));

    // The modern attempt carried per-request metadata; the legacy re-send is the
    // caller's original params, with no `_meta` a legacy server would reject.
    assert!(seen[0].body["params"]["_meta"].is_object());
    assert!(
        seen[1].body["params"].get("_meta").is_none(),
        "the legacy re-send must not carry modern _meta: {}",
        seen[1].body["params"]
    );
}

#[tokio::test]
async fn a_bare_404_falls_back_rather_than_failing() {
    let (url, log) = spawn(Server::NoModernEndpoint).await;
    let transport = DualEraHttpTransport::start(DualEraHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    assert!(matches!(
        call(&read, &write, RequestId::Num(1)).await,
        JsonRpcMessage::Response(_)
    ));
    assert_eq!(transport.era(), Some(ProtocolEra::Legacy));
    assert_eq!(log.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn once_legacy_is_cached_later_requests_skip_the_modern_attempt() {
    let (url, log) = spawn(Server::Legacy).await;
    let transport = DualEraHttpTransport::start(DualEraHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    call(&read, &write, RequestId::Num(1)).await;
    assert_eq!(log.lock().unwrap().len(), 2); // probe + fallback

    call(&read, &write, RequestId::Num(2)).await;
    let seen = log.lock().unwrap().clone();
    assert_eq!(
        seen.len(),
        3,
        "the second call should cost one request, not a fresh probe"
    );
    assert!(!seen[2].modern);
}

#[tokio::test]
async fn an_auth_failure_does_not_decide_the_era() {
    // The bug this guards: treating any answered request as proof of a modern
    // peer would pin the endpoint on the strength of a 401.
    let (url, log) = spawn(Server::Unauthorised).await;
    let transport = DualEraHttpTransport::start(DualEraHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    match call(&read, &write, RequestId::Num(1)).await {
        JsonRpcMessage::Error(e) => assert!(e.error.message.contains("401"), "{}", e.error.message),
        other => panic!("expected the auth error to surface, got {other:?}"),
    }
    assert_eq!(
        transport.era(),
        None,
        "a 401 proves nothing about which protocol the peer speaks"
    );

    // And it must not have been mistaken for legacy either — no fallback resend.
    assert_eq!(log.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_modern_rejection_identifies_a_modern_server() {
    // -32022 is a rejection, but only a modern server can produce it, so the
    // client must stay modern and surface the error with its `supported` list
    // rather than falling back.
    let (url, log) = spawn(Server::WrongVersion).await;
    let transport = DualEraHttpTransport::start(DualEraHttpParameters::new(&url).unwrap()).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    match call(&read, &write, RequestId::Num(1)).await {
        JsonRpcMessage::Error(e) => {
            assert_eq!(e.error.code, UNSUPPORTED_PROTOCOL_VERSION);
            assert_eq!(
                e.error.data.as_ref().unwrap()["supported"],
                json!(["2026-07-28"])
            );
        }
        other => panic!("expected -32022 to reach the caller, got {other:?}"),
    }
    assert_eq!(transport.era(), Some(ProtocolEra::Modern));
    assert_eq!(log.lock().unwrap().len(), 1, "must not fall back");
}

#[tokio::test]
async fn pinning_legacy_never_sends_a_modern_request() {
    let (url, log) = spawn(Server::Modern).await;
    let params = DualEraHttpParameters::new(&url)
        .unwrap()
        .with_mode(EraMode::Legacy);
    let transport = DualEraHttpTransport::start(params).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    assert!(matches!(
        call(&read, &write, RequestId::Num(1)).await,
        JsonRpcMessage::Response(_)
    ));

    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert!(
        !seen[0].modern,
        "a legacy pin must not probe, even against a modern server"
    );
}

#[tokio::test]
async fn pinning_modern_surfaces_the_error_instead_of_falling_back() {
    // Pinned modern against a legacy server: the point of pinning is that the
    // client does not second-guess it, so the 400 reaches the caller.
    let (url, log) = spawn(Server::Legacy).await;
    let params = DualEraHttpParameters::new(&url)
        .unwrap()
        .with_mode(EraMode::Modern);
    let transport = DualEraHttpTransport::start(params).unwrap();
    let (read, write) = transport.get_streams().await.unwrap();

    assert!(matches!(
        call(&read, &write, RequestId::Num(1)).await,
        JsonRpcMessage::Error(_)
    ));
    let seen = log.lock().unwrap().clone();
    assert_eq!(seen.len(), 1, "a modern pin must not fall back");
    assert!(seen[0].modern);
}
