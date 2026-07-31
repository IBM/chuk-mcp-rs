//! The standalone server-to-client stream over Streamable HTTP.
//!
//! A legacy server that needs to ask the client something pushes the request on
//! a `GET` event stream, not on the POST that carried the call. The mock here
//! does exactly that: it holds a `tools/call` open, asks over `GET`, and only
//! answers the call once the client has answered the question.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;

use chuk_mcp::client::input::AcceptDefaults;
use chuk_mcp::client::McpClient;
use chuk_mcp::transports::http::{StreamableHttpParameters, StreamableHttpTransport};

const SESSION_ID: &str = "listen-session-1";
const TOOL_NAME: &str = "needs_input";
const PUSH_ID: &str = "server-push-1";
const DEFAULT_NAME: &str = "octocat";
const FINAL_TEXT: &str = "done";

/// How long the mock holds its `GET` stream open before closing it.
const STREAM_HOLD: Duration = Duration::from_millis(500);
/// How long the test waits for the client to answer.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(10);

/// What the client sent back, and a way to wait for it.
#[derive(Default)]
struct Answers {
    elicit_response: Mutex<Option<Value>>,
    arrived: Notify,
}

fn elicit_push() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": PUSH_ID,
        "method": "elicitation/create",
        "params": {
            "mode": "form",
            "message": "Who are you?",
            "requestedSchema": {
                "type": "object",
                "properties": {"name": {"type": "string", "default": DEFAULT_NAME}},
                "required": ["name"],
            },
        },
    })
}

async fn spawn() -> (String, Arc<Answers>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let answers = Arc::new(Answers::default());

    let served = answers.clone();
    tokio::spawn(async move {
        loop {
            let Ok((socket, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(handle(socket, served.clone()));
        }
    });

    (format!("http://{address}/mcp"), answers)
}

async fn handle(mut socket: TcpStream, answers: Arc<Answers>) {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];

    let (head, body) = loop {
        let read = match socket.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(read) => read,
        };
        buffer.extend_from_slice(&chunk[..read]);
        let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&buffer[..end]).to_string();
        let length = head
            .lines()
            .find(|line| line.to_lowercase().starts_with("content-length:"))
            .and_then(|line| line.split(':').nth(1))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let start = end + 4;
        if buffer.len() - start >= length {
            break (head, buffer[start..start + length].to_vec());
        }
    };

    // The server-to-client stream: ask here, not on the POST.
    if head.starts_with("GET ") {
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
                       Cache-Control: no-cache\r\nConnection: close\r\n\r\n";
        let _ = socket.write_all(headers.as_bytes()).await;
        let event = format!("event: message\ndata: {}\n\n", elicit_push());
        let _ = socket.write_all(event.as_bytes()).await;
        let _ = socket.flush().await;
        tokio::time::sleep(STREAM_HOLD).await;
        return;
    }

    let message: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();

    // The client's answer to the pushed request.
    if method.is_empty() && message.get("id").and_then(Value::as_str) == Some(PUSH_ID) {
        *answers.elicit_response.lock().unwrap() = message.get("result").cloned();
        answers.arrived.notify_waiters();
        respond(&mut socket, 202, "Accepted", "").await;
        return;
    }

    let id = message.get("id").cloned().unwrap_or(Value::Null);
    match method {
        "initialize" => {
            let payload = json!({
                "jsonrpc": "2.0", "id": id,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "serverInfo": {"name": "listen-mock", "version": "1.0.0"},
                    "capabilities": {"tools": {}},
                },
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: {SESSION_ID}\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                payload.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
        "tools/call" => {
            // Hold the call open until the client has answered the question
            // asked on the other stream — the shape the specification's flow
            // diagram describes.
            let waited = tokio::time::timeout(ANSWER_TIMEOUT, async {
                loop {
                    if answers.elicit_response.lock().unwrap().is_some() {
                        return;
                    }
                    let notified = answers.arrived.notified();
                    if answers.elicit_response.lock().unwrap().is_some() {
                        return;
                    }
                    notified.await;
                }
            })
            .await;

            let text = if waited.is_ok() {
                FINAL_TEXT
            } else {
                "timed out"
            };
            let payload = json!({
                "jsonrpc": "2.0", "id": id,
                "result": {"content": [{"type": "text", "text": text}]},
            })
            .to_string();
            respond(&mut socket, 200, "OK", &payload).await;
        }
        _ => {
            let payload = json!({"jsonrpc": "2.0", "id": id, "result": {}}).to_string();
            respond(&mut socket, 200, "OK", &payload).await;
        }
    }
}

async fn respond(socket: &mut TcpStream, status: u16, reason: &str, payload: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
        payload.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
}

#[tokio::test]
async fn a_request_pushed_on_the_get_stream_is_answered() {
    let (url, answers) = spawn().await;

    let transport =
        StreamableHttpTransport::start(StreamableHttpParameters::new(&url).unwrap()).unwrap();
    let mut client = McpClient::new(transport);
    client.set_input_handler(Arc::new(AcceptDefaults));
    client.initialize().await.expect("initialize");

    let result = client
        .call_tool(TOOL_NAME, json!({}))
        .await
        .expect("the call completes once the question is answered");

    // The server only returns this after the client answered on the other
    // stream, so the text proves the whole round trip.
    assert_eq!(result.text(), FINAL_TEXT);

    // And the answer was built from the schema's default, per SEP-1034.
    let answered = answers
        .elicit_response
        .lock()
        .unwrap()
        .clone()
        .expect("the client answered the pushed request");
    assert_eq!(
        answered,
        json!({"action": "accept", "content": {"name": DEFAULT_NAME}})
    );
}

#[tokio::test]
async fn a_server_without_a_get_stream_is_not_retried_forever() {
    // 405 is the documented "no such stream" answer; the listener must stop
    // rather than reconnect in a loop against a server that will never serve it.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let gets = Arc::new(Mutex::new(0usize));

    let counted = gets.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let counted = counted.clone();
            tokio::spawn(async move {
                let mut chunk = [0u8; 2048];
                let Ok(read) = socket.read(&mut chunk).await else {
                    return;
                };
                let raw = String::from_utf8_lossy(&chunk[..read]).to_string();
                let head = raw.clone();
                if head.starts_with("GET ") {
                    *counted.lock().unwrap() += 1;
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n",
                        )
                        .await;
                } else {
                    // Echo the client's own id: a hardcoded one never matches,
                    // and the client would wait out its full timeout.
                    let id = raw
                        .rsplit_once("\r\n\r\n")
                        .and_then(|(_, body)| serde_json::from_str::<Value>(body).ok())
                        .and_then(|message| message.get("id").cloned())
                        .unwrap_or(Value::Null);
                    let payload = json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "protocolVersion": "2025-06-18",
                            "serverInfo": {"name": "no-get", "version": "1.0.0"},
                            "capabilities": {},
                        },
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: s\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
                let _ = socket.flush().await;
            });
        }
    });

    let url = format!("http://{address}/mcp");
    let transport =
        StreamableHttpTransport::start(StreamableHttpParameters::new(&url).unwrap()).unwrap();
    let mut client = McpClient::new(transport);
    let _ = client.initialize().await;

    tokio::time::sleep(Duration::from_millis(800)).await;
    let attempts = *gets.lock().unwrap();
    assert!(
        attempts <= 1,
        "the listener retried a 405 {attempts} times; it must give up"
    );
}

#[tokio::test]
async fn a_transient_get_failure_is_retried() {
    // A 503 says "not now", not "never" — unlike a 405. The listener must come
    // back, or one blip would silently cost the connection its only channel for
    // server-initiated requests for the rest of its life.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let gets = Arc::new(Mutex::new(0usize));

    let counted = gets.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let counted = counted.clone();
            tokio::spawn(async move {
                let mut chunk = [0u8; 2048];
                let Ok(read) = socket.read(&mut chunk).await else {
                    return;
                };
                let raw = String::from_utf8_lossy(&chunk[..read]).to_string();
                if raw.starts_with("GET ") {
                    *counted.lock().unwrap() += 1;
                    let _ = socket
                        .write_all(
                            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n",
                        )
                        .await;
                } else {
                    let id = raw
                        .rsplit_once("\r\n\r\n")
                        .and_then(|(_, body)| serde_json::from_str::<Value>(body).ok())
                        .and_then(|message| message.get("id").cloned())
                        .unwrap_or(Value::Null);
                    let payload = json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "protocolVersion": "2025-06-18",
                            "serverInfo": {"name": "flaky", "version": "1.0.0"},
                            "capabilities": {},
                        },
                    })
                    .to_string();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMcp-Session-Id: s\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                        payload.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                }
                let _ = socket.flush().await;
            });
        }
    });

    let url = format!("http://{address}/mcp");
    let transport =
        StreamableHttpTransport::start(StreamableHttpParameters::new(&url).unwrap()).unwrap();
    let mut client = McpClient::new(transport);
    let _ = client.initialize().await;

    tokio::time::sleep(Duration::from_millis(900)).await;
    let attempts = *gets.lock().unwrap();
    assert!(
        attempts >= 2,
        "the listener gave up after {attempts} attempt(s)"
    );
}
