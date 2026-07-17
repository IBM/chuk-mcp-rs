//! The messaging layer: JSON-RPC send/receive plumbing plus the per-feature
//! MCP operations, mirroring `chuk_mcp.protocol.messages`.

pub mod completions;
pub mod initialize;
pub mod logging;
pub mod method;
pub mod notifications;
pub mod ping;
pub mod prompts;
pub mod resources;
pub mod roots;
pub mod sampling;
pub mod send_message;
pub mod tools;

pub use initialize::{send_initialize, send_initialized_notification, InitializeResult};
pub use method::MessageMethod;
pub use ping::send_ping;
pub use prompts::{send_prompts_get, send_prompts_list, GetPromptResult, Prompt};
pub use resources::{
    send_resources_list, send_resources_read, ReadResourceResult, Resource, ResourceContent,
};
pub use send_message::{send_message, CancellationToken, ReadStream, WriteStream};
pub use tools::{send_tools_call, send_tools_list, Tool, ToolResult};
