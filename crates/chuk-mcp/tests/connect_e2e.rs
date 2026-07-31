//! End-to-end tests for the one-call `connect` API.
//!
//! The demo server is legacy, so these prove the friendly path settles a legacy
//! peer correctly. The modern path is covered by the dual-transport tests and
//! the conformance suite.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use chuk_mcp::client::input::DeclineAll;
use chuk_mcp::protocol::envelope::ClientIdentity;
use chuk_mcp::protocol::era::{EraMode, ProtocolEra};
use chuk_mcp::transports::limits::TransportLimits;
use chuk_mcp::{connect, Connect};

/// The demo server binary, as cargo names it for integration tests.
const DEMO_SERVER: &str = env!("CARGO_BIN_EXE_chuk-mcp-demo-server");

/// A tool the demo server registers, and its argument.
const GREET_TOOL: &str = "greet";
const GREET_ARGUMENT: &str = "name";

/// A generous per-message cap, well above anything the demo server sends.
const MAX_BUFFER_SIZE: usize = 1024 * 1024;

#[tokio::test]
async fn connect_takes_a_command_line() {
    let mut client = connect(DEMO_SERVER).await.expect("connect");

    let tools = client.list_tools().await.expect("tools/list");
    assert!(tools.iter().any(|tool| tool.name == GREET_TOOL));

    let result = client
        .call_tool(GREET_TOOL, json!({GREET_ARGUMENT: "world"}))
        .await
        .expect("tools/call");
    assert!(result.text().contains("world"));

    client.close().await.expect("close");
}

#[tokio::test]
async fn connect_reports_the_server_it_settled_with() {
    let mut client = connect(DEMO_SERVER).await.expect("connect");

    let info = client.server_info().cloned().expect("server info");
    assert!(!info.name.is_empty());
    assert!(client.capabilities().is_some());

    client.close().await.expect("close");
}

#[tokio::test]
async fn the_builder_can_pin_the_era() {
    // The demo server is legacy, so pinning legacy must skip the probe and
    // still succeed.
    let mut client = Connect::to_command(DEMO_SERVER, Vec::<String>::new())
        .era(EraMode::Legacy)
        .connect()
        .await
        .expect("connect pinned to legacy");

    assert!(client.ping().await);
    client.close().await.expect("close");
}

#[tokio::test]
async fn the_builder_carries_stdio_options() {
    let mut environment = HashMap::new();
    environment.insert("CHUK_MCP_CONNECT_TEST".to_string(), "1".to_string());

    let mut client = Connect::to_command(DEMO_SERVER, Vec::<String>::new())
        .identity(ClientIdentity::chuk())
        .limits(TransportLimits::default().with_max_buffer_size(MAX_BUFFER_SIZE))
        .env(environment)
        .timeout(Duration::from_secs(10))
        .connect()
        .await
        .expect("connect with options");

    assert_eq!(client.era(), Some(ProtocolEra::Legacy));
    assert!(client.protocol_version().is_some());
    client.close().await.expect("close");
}

#[tokio::test]
async fn an_input_handler_reaches_the_client_and_is_declared() {
    let mut client = Connect::to_command(DEMO_SERVER, Vec::<String>::new())
        .input_handler(Arc::new(DeclineAll))
        .connect()
        .await
        .expect("connect with an input handler");

    // Attached, so a server that asks mid-call has somewhere to be answered.
    assert!(
        client.input_handler().is_some(),
        "the handler did not reach the client"
    );
    client.close().await.expect("close");
}

#[tokio::test]
async fn a_malformed_target_fails_at_connect_time() {
    let outcome = Connect::to("   ").connect().await;
    assert!(outcome.is_err(), "an empty target must not be guessed at");
}
