//! High-level MCP server, mirroring `chuk_mcp.server`.
//!
//! [`McpServer`] holds the registries — tools, resources, prompts — and routes
//! each message to whichever answers it. The registries do the work; this
//! module is the switchboard, and everything it knows about a feature is
//! confined to the module that owns it.

pub mod caching;
pub mod completion;
pub mod context;
pub mod discover;
pub mod dispatch;
pub mod http;
pub mod listeners;
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

pub use caching::{CacheHints, CachePolicy, CacheScope};
pub use completion::{CompletionHandler, CompletionRequest};
pub use context::CallContext;
pub use listeners::{Listeners, NotificationFilter};
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
/// Multi round-trip fields, which sit beside `arguments` rather than inside
/// them — see [`crate::protocol::mrtr`].
const FIELD_INPUT_RESPONSES: &str = "inputResponses";
const FIELD_REQUEST_STATE: &str = "requestState";

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
    /// What this server tells clients about caching its cacheable results.
    cache_policy: CachePolicy,
    /// The `subscriptions/listen` streams currently open.
    listeners: Arc<Listeners>,
    /// How this server decides whether a returned `requestState` is one it
    /// really minted. `None` accepts whatever comes back.
    request_state_validator: Option<RequestStateValidator>,
}

/// Decides whether an echoed `requestState` is intact.
///
/// The 2026-07-28 revision has no session, so everything a server must
/// remember between the rounds of a multi round-trip request travels through
/// the client — which means the client is holding, and could edit, state the
/// server is about to act on. Servers **MUST** reject state that fails
/// integrity verification.
///
/// The scheme is deliberately the server's to choose: a signed token, an
/// encrypted blob, a key into a store it keeps itself. All this library can do
/// is guarantee the check happens before any handler sees the state, which is
/// what registering one of these buys.
pub type RequestStateValidator = Arc<dyn Fn(&str) -> bool + Send + Sync>;

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
            cache_policy: CachePolicy::default(),
            listeners: Arc::new(Listeners::new()),
            request_state_validator: None,
        }
    }

    /// Check every echoed `requestState` with `validator` before acting on it.
    ///
    /// A request whose state fails is refused with `-32602` and never reaches
    /// a handler. See [`RequestStateValidator`] for why this is the server's
    /// decision rather than the library's.
    pub fn with_request_state_validator<F>(mut self, validator: F) -> Self
    where
        F: Fn(&str) -> bool + Send + Sync + 'static,
    {
        self.request_state_validator = Some(Arc::new(validator));
        self
    }

    /// The `subscriptions/listen` streams currently open.
    ///
    /// A server announces changes through this: registering a tool at runtime
    /// does not by itself tell anyone, because only the server knows whether a
    /// change is one clients should re-fetch for.
    ///
    /// ```no_run
    /// # use chuk_mcp::server::McpServer;
    /// # let server = McpServer::new("s", "1.0.0", None);
    /// server.listeners().tools_list_changed();
    /// ```
    pub fn listeners(&self) -> Arc<Listeners> {
        self.listeners.clone()
    }

    /// What this server tells clients about caching its cacheable results.
    ///
    /// The default is deliberately conservative — see [`caching`] for why a
    /// server that knows its tool list is the same for everyone should say so
    /// with [`CachePolicy::with_scope`].
    pub fn with_cache_policy(mut self, policy: CachePolicy) -> Self {
        self.cache_policy = policy;
        self
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

    /// Register an interactive tool that cannot run unless the client declared
    /// the given capabilities.
    ///
    /// `requires` names `ClientCapabilities` fields — `"sampling"`,
    /// `"elicitation"`, `"roots"`. A 2026-era request that did not declare
    /// them is refused with `-32021` before the handler runs, because a server
    /// **MUST NOT** rely on a capability the request never claimed.
    pub fn register_tool_requiring<F, Fut>(
        &mut self,
        name: &str,
        schema: Value,
        description: &str,
        requires: &[&str],
        handler: F,
    ) where
        F: Fn(Value, CallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.tools
            .insert_requiring(name, schema, description, requires, handler);
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

    /// Register a prompt whose handler shapes its own result.
    ///
    /// `prompts/get` is one of the three requests a 2026-era server may answer
    /// with an `input_required` result, and that is not a list of messages —
    /// so a prompt that might ask for something before rendering returns the
    /// result itself. See [`crate::protocol::mrtr`].
    pub fn register_raw_prompt<F, Fut>(
        &mut self,
        name: &str,
        description: &str,
        arguments: Vec<crate::protocol::messages::prompts::PromptArgument>,
        handler: F,
    ) where
        F: Fn(serde_json::Map<String, Value>, CallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        prompts::register_raw(&mut self.prompts, name, description, arguments, handler);
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

    /// A registered tool's `inputSchema`, for the transport checks that need
    /// to know which parameters the tool asked to be mirrored into headers.
    pub(crate) fn tool_schema(&self, name: &str) -> Option<&Value> {
        self.tools.schema(name)
    }

    /// Which capabilities this call needs that the request did not declare.
    ///
    /// Empty for a legacy request: that era declares its capabilities once
    /// through `initialize`, so there is nothing per-request to check them
    /// against and refusing on that basis would break it.
    pub(crate) fn missing_capabilities(&self, message: &JsonRpcMessage) -> Vec<String> {
        if message.method() != Some(MessageMethod::TOOLS_CALL)
            || !modern::is_modern_request(message)
        {
            return Vec::new();
        }
        let params = params_object(message);
        let Some(name) = params.get(FIELD_NAME).and_then(Value::as_str) else {
            return Vec::new();
        };
        let required = self.tools.requires(name);
        if required.is_empty() {
            return Vec::new();
        }
        modern::undeclared(message, required)
    }

    /// Whether answering this message needs a channel back to the client while
    /// it is handled — a call to a tool that talks as it works.
    ///
    /// A transport asks before handling, because it decides then whether the
    /// answer can be a single message or has to be a stream.
    pub fn needs_stream(&self, message: &JsonRpcMessage) -> bool {
        // A subscription *is* a stream: it produces nothing at all until
        // something changes, and answering it with a single body would close
        // the very channel it exists to open.
        if message.method() == Some(MessageMethod::SUBSCRIPTIONS_LISTEN) {
            return true;
        }
        if message.method() != Some(MessageMethod::TOOLS_CALL) {
            return false;
        }
        // A call that will be refused for a capability the client never
        // declared has nothing to stream: it produces one error and stops.
        // Deciding that here rather than after the stream is open is what lets
        // the refusal carry its `400`, since an event stream is a `200` the
        // moment it starts.
        if !self.missing_capabilities(message).is_empty() {
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
