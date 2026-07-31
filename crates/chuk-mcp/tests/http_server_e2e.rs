//! Our client against our server, over real HTTP, in both eras.
//!
//! Every HTTP test until now drove a hand-written mock on one side. This is
//! the first that has both real implementations on a socket, which is the
//! check that a mock written to match our own reading cannot provide.

use std::time::Duration;

use serde_json::json;

use chuk_mcp::protocol::era::{EraMode, ProtocolEra};
use chuk_mcp::protocol::types::capabilities::{ServerCapabilities, ToolsCapability};
use chuk_mcp::protocol::versioning;
use chuk_mcp::server::http::serve_on;
use chuk_mcp::server::McpServer;
use chuk_mcp::Connect;

const TOOL_NAME: &str = "greet";
const TOOL_ARGUMENT: &str = "name";
const SERVER_NAME: &str = "http-e2e-server";

/// How long to let the listener bind before connecting.
const SETTLE: Duration = Duration::from_millis(50);

fn server() -> McpServer {
    let capabilities = ServerCapabilities {
        tools: Some(ToolsCapability {
            list_changed: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    };
    let mut server = McpServer::new(SERVER_NAME, "1.0.0", Some(capabilities));
    server.register_tool(
        TOOL_NAME,
        json!({
            "type": "object",
            "properties": {TOOL_ARGUMENT: {"type": "string"}},
            "required": [TOOL_ARGUMENT],
        }),
        "Greet someone",
        |arguments| async move {
            let name = arguments
                .get(TOOL_ARGUMENT)
                .and_then(|value| value.as_str())
                .unwrap_or("world");
            Ok(json!(format!("Hello, {name}!")))
        },
    );
    server
}

/// Start the server on an ephemeral port and return its URL.
///
/// Bound before serving so the port is known: asking the OS for one and then
/// racing the client against the bind is how these tests turn flaky.
async fn spawn() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("local address");

    tokio::spawn(async move {
        let _ = serve_on(server(), listener).await;
    });
    tokio::time::sleep(SETTLE).await;

    format!("http://{address}/mcp")
}

#[tokio::test]
async fn a_modern_client_talks_to_our_http_server() {
    let url = spawn().await;

    let client = Connect::to_url(&url)
        .era(EraMode::Modern)
        .connect()
        .await
        .expect("connect over HTTP");

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

    let result = client
        .call_tool(TOOL_NAME, json!({TOOL_ARGUMENT: "modern"}))
        .await
        .expect("tools/call");
    assert!(result.text().contains("Hello, modern!"));
    // The server stamps it, so a modern caller never sees a bare result.
    assert_eq!(result.result_type, "complete");
}

#[tokio::test]
async fn a_legacy_client_talks_to_the_same_server() {
    let url = spawn().await;

    let client = Connect::to_url(&url)
        .era(EraMode::Legacy)
        .connect()
        .await
        .expect("connect over HTTP");

    assert_eq!(client.era(), Some(ProtocolEra::Legacy));
    assert_eq!(
        client.server_info().map(|info| info.name.as_str()),
        Some(SERVER_NAME)
    );

    let result = client
        .call_tool(TOOL_NAME, json!({TOOL_ARGUMENT: "legacy"}))
        .await
        .expect("tools/call");
    assert!(result.text().contains("Hello, legacy!"));
}

#[tokio::test]
async fn detection_settles_our_server_as_modern() {
    // No pin: the client probes, and the server answers `server/discover`.
    let url = spawn().await;

    let client = Connect::to_url(&url).connect().await.expect("connect");
    assert_eq!(client.era(), Some(ProtocolEra::Modern));

    let result = client
        .call_tool(TOOL_NAME, json!({TOOL_ARGUMENT: "detected"}))
        .await
        .expect("tools/call");
    assert!(result.text().contains("Hello, detected!"));
}

// --- the HTTP surface itself, driven with real requests -------------------

/// The server's base URL and its `/mcp` endpoint.
async fn spawn_raw() -> (String, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let address = listener.local_addr().expect("local address");
    tokio::spawn(async move {
        let _ = serve_on(server(), listener).await;
    });
    tokio::time::sleep(SETTLE).await;
    (format!("http://{address}"), format!("http://{address}/mcp"))
}

#[tokio::test]
async fn the_http_surface_answers_each_verb_as_it_should() {
    let (base, mcp) = spawn_raw().await;
    let http = reqwest::Client::new();

    // GET is the server-to-client stream. This server answers everything on
    // the POST that asked, so it says so rather than holding a connection
    // open forever with nothing to send.
    let response = http.get(&mcp).send().await.expect("GET");
    assert_eq!(response.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);

    // Ending a session is a legacy concern and dropping the state is all
    // there is to it.
    let response = http.delete(&mcp).send().await.expect("DELETE");
    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);

    // Anything but /mcp is not ours to answer.
    let response = http
        .post(format!("{base}/somewhere-else"))
        .body("{}")
        .send()
        .await
        .expect("POST elsewhere");
    assert_eq!(response.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_malformed_body_gets_a_readable_error_not_a_dropped_connection() {
    let (_base, mcp) = spawn_raw().await;
    let http = reqwest::Client::new();

    let response = http
        .post(&mcp)
        .body("not json at all")
        .send()
        .await
        .expect("POST");
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    // The reason travels with it: a bare 400 leaves the caller guessing.
    let body = response.text().await.expect("a body");
    assert!(
        body.contains("malformed"),
        "the rejection explained nothing: {body}"
    );
}

#[tokio::test]
async fn a_notification_is_accepted_without_a_reply() {
    let (_base, mcp) = spawn_raw().await;

    let response = reqwest::Client::new()
        .post(&mcp)
        .json(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .send()
        .await
        .expect("POST");

    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    assert!(response.text().await.expect("a body").is_empty());
}

#[tokio::test]
async fn binding_a_port_we_cannot_have_is_reported_rather_than_panicking() {
    // Port 1 needs privileges this test does not have; the point is that a
    // failure to bind is an error the caller can act on.
    let outcome =
        chuk_mcp::server::http::serve_http(server(), "127.0.0.1:1".parse().unwrap()).await;
    assert!(outcome.is_err(), "binding a privileged port must fail");
}

#[tokio::test]
async fn a_body_that_is_not_utf8_is_refused_rather_than_lossily_decoded() {
    let (_base, mcp) = spawn_raw().await;

    // Lone continuation bytes: valid to send, impossible to read as text.
    let response = reqwest::Client::new()
        .post(&mcp)
        .body(vec![0x80u8, 0x81, 0x82])
        .send()
        .await
        .expect("POST");

    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(response.text().await.expect("a body").contains("UTF-8"));
}

#[tokio::test]
async fn a_connection_that_dies_mid_request_does_not_take_the_server_with_it() {
    use tokio::io::AsyncWriteExt;

    let (base, mcp) = spawn_raw().await;
    let address = base.trim_start_matches("http://").to_string();

    // Announce a body and then vanish. The connection's failure is one
    // client's problem, not the server's.
    let mut socket = tokio::net::TcpStream::connect(&address)
        .await
        .expect("connect");
    socket
        .write_all(b"POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Length: 100\r\n\r\n{")
        .await
        .expect("partial write");
    drop(socket);
    tokio::time::sleep(SETTLE).await;

    // Still serving.
    let response = reqwest::Client::new()
        .post(&mcp)
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {}}))
        .send()
        .await
        .expect("the server is still up");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
}

#[tokio::test]
async fn serve_http_binds_and_serves_on_its_own() {
    // A free port, released just before handing it to `serve_http` — the
    // convenience entry point binds for itself, so there is no listener to
    // pass it.
    let probe = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("probe bind");
    let address = probe.local_addr().expect("address");
    drop(probe);

    tokio::spawn(async move {
        let _ = chuk_mcp::server::http::serve_http(server(), address).await;
    });
    tokio::time::sleep(SETTLE).await;

    let client = Connect::to_url(format!("http://{address}/mcp"))
        .connect()
        .await
        .expect("connect to a server that bound itself");
    assert_eq!(client.era(), Some(ProtocolEra::Modern));
}
