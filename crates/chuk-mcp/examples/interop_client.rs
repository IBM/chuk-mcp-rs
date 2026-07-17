//! Connect to any stdio MCP server and exercise tools/resources.
//!
//! Usage: cargo run --example interop_client -- <command> [args...]

use serde_json::json;

use chuk_mcp::client::connect_to_server;
use chuk_mcp::transports::stdio::StdioParameters;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut argv = std::env::args().skip(1);
    let command = argv.next().expect("usage: interop_client <command> [args...]");
    let params = StdioParameters::new(command, argv);

    let mut client = connect_to_server(params).await?;
    let info = client.server_info.clone().expect("server info");
    println!("initialized: {} v{}", info.name, info.version);

    println!("ping: {}", client.ping().await);

    let tools = client.list_tools().await?;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    println!("tools: {names:?}");

    if names.contains(&"greet") {
        let result = client.call_tool("greet", json!({"name": "RustClient"})).await?;
        println!("greet: {}", result.text());
    }
    if names.contains(&"add") {
        let result = client.call_tool("add", json!({"a": 20, "b": 22})).await?;
        println!("add: {}", result.text());
    }

    client.close().await?;
    println!("RUST-CLIENT -> SERVER: PASSED");
    Ok(())
}
