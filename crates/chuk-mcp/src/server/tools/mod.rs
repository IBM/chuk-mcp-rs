//! The tools a server offers, and what calling one produces.

pub mod content;

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use serde_json::{json, Value};

use super::context::CallContext;

/// An async tool handler: arguments and a call context in, JSON value (or
/// error text) out.
///
/// String results become text content; content blocks are passed through;
/// other values are pretty-printed JSON. See [`content`].
pub type ToolHandler = Arc<
    dyn Fn(Value, CallContext) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

/// `tools/list` field names.
const FIELD_NAME: &str = "name";
const FIELD_DESCRIPTION: &str = "description";
const FIELD_INPUT_SCHEMA: &str = "inputSchema";
const FIELD_TOOLS: &str = "tools";

struct RegisteredTool {
    handler: ToolHandler,
    schema: Value,
    description: String,
    /// Whether this tool was registered as one that talks to the client while
    /// it runs. The transport needs to know before the call starts, because it
    /// decides whether the answer can be streamed.
    interactive: bool,
    /// Client capabilities this tool cannot run without, by their
    /// `ClientCapabilities` field names (`"sampling"`, `"elicitation"`, …).
    ///
    /// Checked before the handler runs: a 2026-era server **MUST NOT** rely on
    /// a capability the request did not declare, and finding out halfway
    /// through a call is too late to answer cleanly.
    requires: Vec<String>,
}

/// The tools a server offers, by name.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, RegisteredTool>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a tool that answers without speaking first.
    pub fn insert<F, Fut>(&mut self, name: &str, schema: Value, description: &str, handler: F)
    where
        F: Fn(Value) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.insert_handler(
            name,
            schema,
            description,
            false,
            Arc::new(move |args, _context| Box::pin(handler(args))),
        );
    }

    /// Register a tool that may report progress, log, sample or elicit while
    /// it runs.
    pub fn insert_interactive<F, Fut>(
        &mut self,
        name: &str,
        schema: Value,
        description: &str,
        handler: F,
    ) where
        F: Fn(Value, CallContext) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        self.insert_handler(
            name,
            schema,
            description,
            true,
            Arc::new(move |args, context| Box::pin(handler(args, context))),
        );
    }

    /// Register an interactive tool that cannot run unless the client declared
    /// the given capabilities.
    ///
    /// `requires` names `ClientCapabilities` fields — `"sampling"`,
    /// `"elicitation"`, `"roots"`. A modern request that did not declare them
    /// is refused with `-32021` before the handler runs.
    pub fn insert_requiring<F, Fut>(
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
        self.insert_handler(
            name,
            schema,
            description,
            true,
            Arc::new(move |args, context| Box::pin(handler(args, context))),
        );
        if let Some(tool) = self.tools.get_mut(name) {
            tool.requires = requires.iter().map(|name| name.to_string()).collect();
        }
    }

    fn insert_handler(
        &mut self,
        name: &str,
        schema: Value,
        description: &str,
        interactive: bool,
        handler: ToolHandler,
    ) {
        self.tools.insert(
            name.to_string(),
            RegisteredTool {
                handler,
                schema,
                description: description.to_string(),
                interactive,
                requires: Vec::new(),
            },
        );
        tracing::debug!("Registered tool: {name}");
    }

    /// This tool's declared `inputSchema`.
    pub fn schema(&self, name: &str) -> Option<&Value> {
        self.tools.get(name).map(|tool| &tool.schema)
    }

    /// The client capabilities this tool cannot run without.
    pub fn requires(&self, name: &str) -> &[String] {
        self.tools
            .get(name)
            .map(|tool| tool.requires.as_slice())
            .unwrap_or_default()
    }

    /// Whether a tool by this name is registered and talks while it runs.
    ///
    /// Asked before the call, so a transport can decide whether it needs a
    /// stream to carry what the tool will say.
    pub fn is_interactive(&self, name: &str) -> bool {
        self.tools.get(name).is_some_and(|tool| tool.interactive)
    }

    /// Whether anything is registered under this name.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    /// The `tools/list` result.
    pub fn list_result(&self) -> Value {
        let tools: Vec<Value> = self
            .tools
            .iter()
            .map(|(name, tool)| {
                json!({
                    FIELD_NAME: name,
                    FIELD_DESCRIPTION: tool.description,
                    FIELD_INPUT_SCHEMA: tool.schema,
                })
            })
            .collect();
        json!({FIELD_TOOLS: tools})
    }

    /// Call a tool, or `None` if no such tool is registered.
    ///
    /// A handler that fails still produces a result — see
    /// [`content::error_result`] for why that is not a JSON-RPC error.
    pub async fn call(&self, name: &str, arguments: Value, context: CallContext) -> Option<Value> {
        let tool = self.tools.get(name)?;
        Some(match (tool.handler)(arguments, context).await {
            Ok(value) => content::call_result(value),
            Err(error) => {
                tracing::error!("Tool execution error for {name}: {error}");
                content::error_result(&error)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn greeting_registry() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        registry.insert(
            "greet",
            json!({"type": "object"}),
            "Say hello",
            |args: Value| async move {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("world");
                Ok(json!(format!("Hello, {name}!")))
            },
        );
        registry
    }

    #[tokio::test]
    async fn a_tool_can_declare_the_capabilities_it_needs() {
        let mut registry = ToolRegistry::new();
        registry.insert_requiring(
            "needs_sampling",
            json!({"type": "object"}),
            "Needs the client to sample",
            &["sampling"],
            |_args, _context| async { Ok(json!("ran")) },
        );

        assert_eq!(
            registry.requires("needs_sampling"),
            &["sampling".to_string()]
        );
        // A tool that declared none, and a name nobody registered, both have
        // nothing to check — and neither is an error.
        assert!(registry.requires("greet").is_empty());
        assert!(registry.requires("never-registered").is_empty());
        // Such a tool talks to the client, so a transport must stream it.
        assert!(registry.is_interactive("needs_sampling"));
    }

    #[test]
    fn a_tool_schema_is_readable_for_header_promotion() {
        let registry = greeting_registry();
        assert_eq!(registry.schema("greet"), Some(&json!({"type": "object"})));
        assert_eq!(registry.schema("never-registered"), None);
    }

    #[tokio::test]
    async fn a_registered_tool_is_listed_and_callable() {
        let registry = greeting_registry();

        let listed = registry.list_result();
        assert_eq!(listed[FIELD_TOOLS][0][FIELD_NAME], json!("greet"));
        assert_eq!(
            listed[FIELD_TOOLS][0][FIELD_DESCRIPTION],
            json!("Say hello")
        );
        assert!(listed[FIELD_TOOLS][0][FIELD_INPUT_SCHEMA].is_object());

        let result = registry
            .call("greet", json!({"name": "Rust"}), CallContext::detached())
            .await
            .expect("a registered tool answers");
        assert_eq!(result["content"][0]["text"], json!("Hello, Rust!"));
    }

    #[tokio::test]
    async fn an_unregistered_tool_answers_nothing_at_all() {
        // `None` rather than an error result: whether an unknown tool is a
        // JSON-RPC error is the caller's decision, not the registry's.
        assert!(greeting_registry().contains("greet"));
        assert!(!greeting_registry().contains("missing"));
        assert!(greeting_registry()
            .call("missing", json!({}), CallContext::detached())
            .await
            .is_none());
    }

    #[tokio::test]
    async fn a_failing_tool_produces_an_error_result() {
        let mut registry = ToolRegistry::new();
        registry.insert("boom", json!({}), "Fails", |_| async {
            Err("the kettle is empty".to_string())
        });

        let result = registry
            .call("boom", json!({}), CallContext::detached())
            .await
            .expect("a registered tool answers");
        assert_eq!(result["isError"], json!(true));
        assert_eq!(result["content"][0]["text"], json!("the kettle is empty"));
    }

    #[tokio::test]
    async fn an_interactive_tool_is_marked_and_receives_its_context() {
        let mut registry = ToolRegistry::new();
        registry.insert_interactive(
            "ask",
            json!({}),
            "Talks",
            |_, context: CallContext| async move {
                // The context is real, even when detached.
                Ok(json!(format!("connected: {}", context.is_connected())))
            },
        );

        assert!(registry.is_interactive("ask"));
        // A plain tool is not, and neither is one that does not exist.
        assert!(!greeting_registry().is_interactive("greet"));
        assert!(!registry.is_interactive("missing"));

        let result = registry
            .call("ask", json!({}), CallContext::detached())
            .await
            .expect("a registered tool answers");
        assert_eq!(result["content"][0]["text"], json!("connected: false"));
    }

    #[tokio::test]
    async fn registering_the_same_name_twice_keeps_the_later_tool() {
        let mut registry = greeting_registry();
        registry.insert("greet", json!({}), "Replaced", |_| async {
            Ok(json!("replaced"))
        });

        assert_eq!(
            registry.list_result()[FIELD_TOOLS]
                .as_array()
                .expect("a list")
                .len(),
            1
        );
        let result = registry
            .call("greet", json!({}), CallContext::detached())
            .await
            .expect("a registered tool answers");
        assert_eq!(result["content"][0]["text"], json!("replaced"));
    }
}
