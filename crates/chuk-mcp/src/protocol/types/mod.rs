//! MCP protocol type definitions, mirroring `chuk_mcp.protocol.types`.

pub mod capabilities;
pub mod content;
pub mod elicitation;
pub mod errors;
pub mod info;
pub mod tools;

pub use capabilities::{ClientCapabilities, ServerCapabilities};
pub use content::{Annotations, Content, ResourceContents, Role};
pub use errors::McpError;
pub use info::{ClientInfo, ServerInfo};
pub use tools::{Tool, ToolInputSchema, ToolResult};
