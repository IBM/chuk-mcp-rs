//! End-to-end test: Rust client ↔ demo server over the stdio transport.

use serde_json::{json, Value};

use chuk_mcp::client::connect_to_server;
use chuk_mcp::transports::stdio::StdioParameters;

fn demo_server_params() -> StdioParameters {
    StdioParameters::new(
        env!("CARGO_BIN_EXE_chuk-mcp-demo-server"),
        Vec::<String>::new(),
    )
}

#[tokio::test]
async fn full_client_server_roundtrip() {
    let mut client = connect_to_server(demo_server_params())
        .await
        .expect("connect + initialize");

    // Initialization populated server metadata.
    let info = client.server_info.clone().expect("server info");
    assert_eq!(info.name, "chuk-mcp-demo");

    // Ping.
    assert!(client.ping().await);

    // Tools.
    let tools = client.list_tools().await.expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, vec!["add", "greet"]);

    let result = client
        .call_tool("greet", json!({"name": "Rust"}))
        .await
        .expect("call greet");
    assert_eq!(result.text(), "Hello, Rust!");

    let result = client
        .call_tool("add", json!({"a": 2, "b": 40}))
        .await
        .expect("call add");
    let parsed: Value = serde_json::from_str(&result.text()).expect("json result");
    assert_eq!(parsed["sum"], json!(42.0));

    // Unknown tool surfaces a non-retryable JSON-RPC error.
    let err = client
        .call_tool("nope", json!({}))
        .await
        .expect_err("unknown tool should fail");
    assert!(!err.is_retryable());

    // Resources.
    let resources = client.list_resources().await.expect("list resources");
    assert_eq!(resources[0].uri, "demo://motd");

    let contents = client
        .read_resource("demo://motd")
        .await
        .expect("read resource");
    assert_eq!(
        contents.contents[0].text.as_deref(),
        Some("chuk-mcp demo server says hi")
    );

    client.close().await.expect("close");
}
