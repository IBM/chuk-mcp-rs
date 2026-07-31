//! Coverage-oriented tests for the high-level client, the server dispatch, and
//! the protocol handler.

// Covers `McpClient::from_settled` deliberately: deprecated in favour of
// `from_profile`, but still supported.
#![allow(deprecated)]
use serde_json::{json, Value};
use tokio::sync::mpsc;

use chuk_mcp::client::McpClient;
use chuk_mcp::protocol::json_rpc::{create_response, parse_message, JsonRpcMessage, RequestId};
use chuk_mcp::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use chuk_mcp::server::{method_handler, McpServer, ProtocolHandler};
use chuk_mcp::transports::Transport;

// --- fake transport with an auto-responding server ----------------------

struct FakeTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
}

impl FakeTransport {
    fn new() -> Self {
        let (inject, incoming) = message_channel(32);
        let (outgoing, mut outgoing_rx) = mpsc::channel::<JsonRpcMessage>(32);
        tokio::spawn(async move {
            while let Some(msg) = outgoing_rx.recv().await {
                let Some(id) = msg.id().cloned() else {
                    continue;
                };
                let result = match msg.method().unwrap_or("") {
                    "initialize" => json!({
                        "protocolVersion": "2025-06-18",
                        "serverInfo": {"name": "fake", "version": "1"},
                        "capabilities": {"tools": {}, "resources": {}}
                    }),
                    "tools/list" => json!({"tools": [{"name": "t", "inputSchema": {}}]}),
                    "tools/call" => json!({"content": [{"type": "text", "text": "called"}]}),
                    "resources/list" => json!({"resources": [{"uri": "u", "name": "n"}]}),
                    "resources/read" => json!({"contents": [{"uri": "u", "text": "body"}]}),
                    "prompts/list" => json!({"prompts": [{"name": "p"}]}),
                    "prompts/get" => json!({"messages": []}),
                    "ping" => json!({}),
                    _ => json!({}),
                };
                let _ = inject
                    .send(JsonRpcMessage::Response(create_response(id, Some(result))))
                    .await;
            }
        });
        FakeTransport { incoming, outgoing }
    }
}

#[async_trait::async_trait]
impl Transport for FakeTransport {
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), chuk_mcp::McpError> {
        Ok((self.incoming.clone(), self.outgoing.clone()))
    }
}

#[tokio::test]
async fn client_full_flow() {
    let mut client = McpClient::new(FakeTransport::new());
    assert!(!client.initialized());

    // methods before initialize error out
    assert!(client.list_tools().await.is_err());

    let init = client.initialize().await.unwrap();
    assert_eq!(init.server_info.name, "fake");
    assert!(client.initialized());
    assert_eq!(client.server_info().unwrap().name, "fake");
    assert!(client.capabilities().is_some());
    // idempotent
    client.initialize().await.unwrap();

    assert_eq!(client.list_tools().await.unwrap()[0].name, "t");
    assert_eq!(
        client.call_tool("t", json!({"x": 1})).await.unwrap().text(),
        "called"
    );
    assert_eq!(client.list_resources().await.unwrap()[0].uri, "u");
    assert_eq!(
        client.read_resource("u").await.unwrap().contents[0]
            .text
            .as_deref(),
        Some("body")
    );
    assert_eq!(client.list_prompts().await.unwrap()[0].name, "p");
    assert!(client
        .get_prompt("p", None)
        .await
        .unwrap()
        .messages
        .is_some());
    assert!(client.ping().await);
    assert!(client.raw_streams().is_ok());
    client.close().await.unwrap();
}

#[tokio::test]
async fn ping_false_before_init() {
    let client = McpClient::new(FakeTransport::new());
    assert!(!client.ping().await); // not initialized -> false
}

#[tokio::test]
async fn client_from_settled_skips_handshake() {
    use chuk_mcp::protocol::types::info::ServerInfo;

    // A dual-era connection settles the era and handshake itself, then hands the
    // client the streams + profile: no initialize() is issued, yet the client is
    // ready and its send_*-backed operations work over the settled streams.
    let transport = FakeTransport::new();
    let (read, write) = transport.get_streams().await.unwrap();
    let client = McpClient::from_settled(
        transport,
        read,
        write,
        Some(ServerInfo::new("modern", "2")),
        None,
    );
    assert!(client.initialized());
    assert_eq!(client.server_info().unwrap().name, "modern");
    assert_eq!(client.list_tools().await.unwrap()[0].name, "t");
}

// --- server dispatch -----------------------------------------------------

fn demo_server() -> McpServer {
    let mut server = McpServer::new("srv", "1.0", None);
    server.register_tool(
        "echo",
        json!({"type": "object"}),
        "echo",
        |args| async move { Ok(args.get("v").cloned().unwrap_or(json!("none"))) },
    );
    server.register_tool("boom", json!({}), "boom", |_| async move {
        Err("kaboom".to_string())
    });
    // format_content variants
    server.register_tool(
        "as_obj",
        json!({}),
        "",
        |_| async move { Ok(json!({"k": "v"})) },
    );
    server.register_tool(
        "as_arr",
        json!({}),
        "",
        |_| async move { Ok(json!(["a", "b"])) },
    );
    server.register_tool("as_num", json!({}), "", |_| async move { Ok(json!(42)) });
    server.register_resource("res://x", "", "desc", "", || async move {
        Ok("resource body".to_string())
    });
    server.register_resource("res://err", "e", "d", "text/plain", || async move {
        Err("read failed".to_string())
    });
    server
}

async fn dispatch(server: &McpServer, value: Value) -> Option<JsonRpcMessage> {
    let msg = parse_message(&value).unwrap();
    server.handle_message(msg, None).await.0
}

#[tokio::test]
async fn server_dispatch_full() {
    let server = demo_server();

    // tools/list lists registered tools (sorted)
    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await
    .unwrap();
    let names: Vec<&str> = resp.result().unwrap()["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"echo"));

    // tools/call: success (string content)
    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "echo", "arguments": {"v": "hi"}}}),
    )
    .await
    .unwrap();
    assert_eq!(resp.result().unwrap()["content"][0]["text"], "hi");

    // format_content: object -> pretty JSON text, array -> flattened, number -> string
    for (tool, expect_contains) in [("as_obj", "\"k\""), ("as_arr", "a"), ("as_num", "42")] {
        let resp = dispatch(
            &server,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": tool}}),
        )
        .await
        .unwrap();
        let text = resp.result().unwrap()["content"][0]["text"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(text.contains(expect_contains), "{tool}: {text}");
    }

    // tools/call: unknown tool -> error
    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "missing"}}),
    )
    .await
    .unwrap();
    assert_eq!(resp.error().unwrap().code, -32602);

    // tools/call: handler error -> internal error
    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "boom"}}),
    )
    .await
    .unwrap();
    assert_eq!(resp.error().unwrap().code, -32603);

    // resources/list + read (success / unknown / handler error)
    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 6, "method": "resources/list"}),
    )
    .await
    .unwrap();
    let uris: Vec<&str> = resp.result().unwrap()["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"res://x"));

    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 7, "method": "resources/read", "params": {"uri": "res://x"}}),
    )
    .await
    .unwrap();
    assert_eq!(
        resp.result().unwrap()["contents"][0]["text"],
        "resource body"
    );

    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 8, "method": "resources/read", "params": {"uri": "nope"}}),
    )
    .await
    .unwrap();
    assert_eq!(resp.error().unwrap().code, -32602);

    let resp = dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 9, "method": "resources/read", "params": {"uri": "res://err"}}),
    )
    .await
    .unwrap();
    assert_eq!(resp.error().unwrap().code, -32603);

    // initialize + ping + unknown method + notification
    let (resp, session) = server
        .handle_message(
            parse_message(&json!({"jsonrpc": "2.0", "id": 10, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "clientInfo": {"name": "c"}}}))
                .unwrap(),
            None,
        )
        .await;
    assert!(resp.is_some());
    assert!(session.is_some());

    assert!(dispatch(
        &server,
        json!({"jsonrpc": "2.0", "id": 11, "method": "ping"})
    )
    .await
    .unwrap()
    .is_response());

    assert_eq!(
        dispatch(
            &server,
            json!({"jsonrpc": "2.0", "id": 12, "method": "bogus/method"})
        )
        .await
        .unwrap()
        .error()
        .unwrap()
        .code,
        -32601
    );

    // notification (no id) -> no response
    assert!(dispatch(
        &server,
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .await
    .is_none());
}

#[tokio::test]
async fn server_serve_rejects_oversized_line() {
    // A client that never terminates its line must not grow the server's
    // buffer without bound.
    let server = demo_server().with_max_buffer_size(1000);
    let input = vec![b'A'; 100_000];
    let mut out: Vec<u8> = Vec::new();

    let err = server
        .serve(tokio::io::BufReader::new(input.as_slice()), &mut out)
        .await
        .expect_err("oversized line should abort the serve loop");

    assert!(err.to_string().contains("maximum buffered size"), "{err}");
    assert!(out.is_empty());
}

#[tokio::test]
async fn server_serve_allows_large_line_under_the_cap() {
    // The cap must not break a legitimately large message.
    let server = demo_server();
    let padding = "x".repeat(100_000);
    let input = format!(
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{"protocolVersion":"2025-06-18","clientInfo":{{"name":"{padding}"}}}}}}"#
    ) + "\n";
    let mut out: Vec<u8> = Vec::new();

    server
        .serve(tokio::io::BufReader::new(input.as_bytes()), &mut out)
        .await
        .unwrap();

    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("protocolVersion"), "{out}");
}

#[tokio::test]
async fn server_serve_over_pipe() {
    let server = demo_server();
    let input = concat!(
        "\n",         // blank line -> skipped
        "not json\n", // invalid -> skipped, no response
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","clientInfo":{"name":"c"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n", // notification -> no response
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        "\n",
    );
    let mut out: Vec<u8> = Vec::new();
    server
        .serve(tokio::io::BufReader::new(input.as_bytes()), &mut out)
        .await
        .unwrap();
    let out = String::from_utf8(out).unwrap();
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].contains("protocolVersion"));
    assert!(lines[1].contains("tools"));
}

// --- protocol handler ----------------------------------------------------

#[tokio::test]
async fn protocol_handler_custom_and_errors() {
    use chuk_mcp::protocol::types::capabilities::ServerCapabilities;
    use chuk_mcp::protocol::types::info::ServerInfo;

    let mut handler =
        ProtocolHandler::new(ServerInfo::new("s", "1"), ServerCapabilities::default());
    assert!(!handler.has_custom_handler("custom/echo"));
    handler.register_method(
        "custom/echo",
        method_handler(|msg, session| async move {
            assert_eq!(session.as_deref(), Some("s1"));
            let id = msg.id().cloned().unwrap();
            Ok((
                Some(JsonRpcMessage::Response(create_response(
                    id,
                    msg.params().cloned(),
                ))),
                Some("sess".to_string()),
            ))
        }),
    );
    assert!(handler.has_custom_handler("custom/echo"));

    let (resp, session) = handler
        .handle_message(
            parse_message(
                &json!({"jsonrpc": "2.0", "id": 1, "method": "custom/echo", "params": {"x": 9}}),
            )
            .unwrap(),
            Some("s1"),
        )
        .await;
    assert_eq!(resp.unwrap().result().unwrap()["x"], 9);
    assert_eq!(session.as_deref(), Some("sess"));

    // handler that errors -> internal error response
    handler.register_method(
        "custom/fail",
        method_handler(|_, _| async move { Err("nope".to_string()) }),
    );
    let (resp, _) = handler
        .handle_message(
            parse_message(&json!({"jsonrpc": "2.0", "id": 2, "method": "custom/fail"})).unwrap(),
            None,
        )
        .await;
    assert_eq!(resp.unwrap().error().unwrap().code, -32603);

    // create_response / create_error_response helpers
    assert!(handler
        .create_response(RequestId::Num(1), Some(json!({})))
        .is_response());
    assert_eq!(
        handler
            .create_error_response(RequestId::Num(1), -32000, "x")
            .error()
            .unwrap()
            .code,
        -32000
    );

    // a batch / response message is ignored
    let (resp, _) = handler
        .handle_message(
            JsonRpcMessage::Response(create_response(RequestId::Num(3), None)),
            None,
        )
        .await;
    assert!(resp.is_none());
}
