//! Connect to any stdio MCP server and exercise tools/resources.
//!
//! Usage: cargo run --example interop_client -- <command> [args...]

use chuk_mcp::prelude::*;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut argv = std::env::args().skip(1);
    let command = argv
        .next()
        .expect("usage: interop_client <command> [args...]");

    let mut client = Connect::to_command(command, argv).connect().await?;
    let info = client.server_info().cloned().expect("server info");
    println!(
        "connected: {} v{} over {}",
        info.name,
        info.version,
        client
            .era()
            .map(|era| era.to_string())
            .unwrap_or_else(|| "an unknown era".to_string()),
    );

    println!("ping: {}", client.ping().await);

    let tools = client.list_tools().await?;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    println!("tools: {names:?}");

    if names.contains(&"greet") {
        let result = client
            .call_tool("greet", json!({"name": "RustClient"}))
            .await?;
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
