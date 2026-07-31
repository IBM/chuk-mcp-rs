//! High-level MCP server, mirroring `chuk_mcp.server`.
//!
//! [`McpServer`] holds the registries — tools, resources, prompts — and routes
//! each message to whichever answers it. The registries do the work; this
//! module is the switchboard, and everything it knows about a feature is
//! confined to the module that owns it.

pub mod completion;
pub mod context;
pub mod discover;
pub mod dispatch;
pub mod http;
pub mod logging;
pub mod modern;
pub mod pending;
pub mod prompts;
pub mod protocol_handler;
pub mod resources;
pub mod serve;
pub mod session;
pub mod tools;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use futures::Future;
use serde_json::Value;

use crate::protocol::json_rpc::JsonRpcMessage;
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::types::capabilities::ServerCapabilities;
use crate::protocol::types::info::ServerInfo;

pub use completion::{CompletionHandler, CompletionRequest};
pub use context::CallContext;
pub use logging::LogLevel;
pub use pending::PendingRequests;
pub use protocol_handler::{method_handler, params_object, HandlerResult, ProtocolHandler};
pub use resources::{ResourceBody, ResourceHandler, TemplateHandler};
pub use session::{SessionInfo, SessionManager};
pub use tools::ToolHandler;

/// Request parameter names this module reads.
const FIELD_NAME: &str = "name";
const FIELD_ARGUMENTS: &str = "arguments";
const FIELD_URI: &str = "uri";
const FIELD_LEVEL: &str = "level";
const FIELD_META: &str = "_meta";
const FIELD_PROGRESS_TOKEN: &str = "progressToken";

/// High-level MCP server: register tools and resources, then feed it
/// messages (e.g. via [`McpServer::run_stdio`]).
pub struct McpServer {
    pub protocol_handler: ProtocolHandler,
    tools: tools::ToolRegistry,
    resources: resources::ResourceRegistry,
    subscriptions: resources::Subscriptions,
    prompts: prompts::PromptRegistry,
    completions: completion::Completions,
    log_level: logging::LogLevelSetting,
    /// Requests this server has put to the client, awaiting their answers.
    pending: Arc<PendingRequests>,
    max_buffer_size: usize,
    client_timeout: Duration,
    /// Optional natural-language guidance for a model on using this server,
    /// returned by `server/discover`.
    instructions: Option<String>,
}

impl McpServer {
    pub fn new(name: &str, version: &str, capabilities: Option<ServerCapabilities>) -> Self {
        let server_info = ServerInfo::new(name, version);
        let capabilities = capabilities.unwrap_or_default();
        tracing::info!("Initialized MCP server: {name} v{version}");
        McpServer {
            protocol_handler: ProtocolHandler::new(server_info, capabilities),
            tools: tools::ToolRegistry::new(),
            resources: resources::ResourceRegistry::new(),
            subscriptions: resources::Subscriptions::new(),
            prompts: prompts::PromptRegistry::new(),
            completions: completion::Completions::new(),
            log_level: logging::LogLevelSetting::new(),
            pending: Arc::new(PendingRequests::new()),
            max_buffer_size: crate::transports::limits::DEFAULT_MAX_BUFFER_SIZE,
            client_timeout: context::DEFAULT_CLIENT_TIMEOUT,
            instructions: None,
        }
    }

    /// Natural-language guidance for a model on how to use this server,
    /// returned by `server/discover`.
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }

    /// Set the maximum bytes buffered for a single inbound message (0 disables).
    ///
    /// Bounds how much a client can send without a newline before the server
    /// gives up, rather than accumulating it all in memory.
    pub fn with_max_buffer_size(mut self, max_buffer_size: usize) -> Self {
        self.max_buffer_size = max_buffer_size;
        self
    }

    /// How long a request put to the client waits for its answer.
    pub fn with_client_timeout(mut self, timeout: Duration) -> Self {
        self.client_timeout = timeout;
        self
    }

    /// Register a tool with its JSON Schema and async handler.
    pub fn register_tool<F, Fut>(
        &mut self,
        name: &str,
        schema: Value,
        description: &str,
        handler: F,
    ) where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.tools.insert(name, schema, description, handler);
    }

    /// Register a tool that talks to the client while it runs — reporting
    /// progress, logging, sampling a model, or asking the user something.
    ///
    /// See [`CallContext`]. The distinction is not cosmetic: a transport needs
    /// to know before the call whether the answer has to be streamed.
    pub fn register_interactive_tool<F, Fut>(
        &mut self,
        name: &str,
        schema: Value,
        description: &str,
        handler: F,
    ) where
        F: Fn(Value, CallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.tools
            .insert_interactive(name, schema, description, handler);
    }

    /// Register a resource with its async handler.
    pub fn register_resource<F, Fut>(
        &mut self,
        uri: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        handler: F,
    ) where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        self.resources
            .insert(uri, name, description, mime_type, false, handler);
    }

    /// Register a resource whose handler returns base64-encoded bytes.
    ///
    /// The body is carried as `blob` rather than `text`, which is what tells
    /// the client it has bytes to decode rather than something to read.
    pub fn register_binary_resource<F, Fut>(
        &mut self,
        uri: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        handler: F,
    ) where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        self.resources
            .insert(uri, name, description, mime_type, true, handler);
    }

    /// Register a family of resources named by a URI template such as
    /// `db://{table}/rows/{id}`.
    pub fn register_resource_template<F, Fut>(
        &mut self,
        uri_template: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        handler: F,
    ) where
        F: Fn(BTreeMap<String, String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        self.resources
            .insert_template(uri_template, name, description, mime_type, false, handler);
    }

    /// [`register_resource_template`](Self::register_resource_template) for a
    /// handler returning base64-encoded bytes.
    pub fn register_binary_resource_template<F, Fut>(
        &mut self,
        uri_template: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        handler: F,
    ) where
        F: Fn(BTreeMap<String, String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        self.resources
            .insert_template(uri_template, name, description, mime_type, true, handler);
    }

    /// Register a prompt: a named template a model can ask to be rendered.
    pub fn register_prompt<F, Fut>(
        &mut self,
        name: &str,
        description: &str,
        arguments: Vec<crate::protocol::messages::prompts::PromptArgument>,
        handler: F,
    ) where
        F: Fn(serde_json::Map<String, Value>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Vec<crate::protocol::messages::prompts::PromptMessage>, String>>
            + Send
            + 'static,
    {
        prompts::register(&mut self.prompts, name, description, arguments, handler);
    }

    /// Supply argument completions for `completion/complete`.
    pub fn register_completion<F, Fut>(&mut self, handler: F)
    where
        F: Fn(Value, String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<String>> + Send + 'static,
    {
        self.completions.set(handler);
    }

    /// The URIs currently subscribed to.
    pub fn subscriptions(&self) -> Vec<String> {
        self.subscriptions.all()
    }

    /// The minimum severity the client has asked to see.
    pub fn log_level(&self) -> LogLevel {
        self.log_level.get()
    }

    /// The requests this server is waiting on the client to answer.
    pub fn pending(&self) -> Arc<PendingRequests> {
        self.pending.clone()
    }

    /// Whether answering this message needs a channel back to the client while
    /// it is handled — a call to a tool that talks as it works.
    ///
    /// A transport asks before handling, because it decides then whether the
    /// answer can be a single message or has to be a stream.
    pub fn needs_stream(&self, message: &JsonRpcMessage) -> bool {
        if message.method() != Some(MessageMethod::TOOLS_CALL) {
            return false;
        }
        params_object(message)
            .get(FIELD_NAME)
            .and_then(Value::as_str)
            .is_some_and(|name| self.tools.is_interactive(name))
    }
}

#[cfg(test)]
mod tests;
