//! Routing one message to whatever answers it.
//!
//! Every arm here is a lookup into a registry and an envelope around what it
//! returned. The knowledge of *how* a feature works lives in that feature's
//! module; what lives here is only which method reaches it.

use serde_json::{json, Value};

use crate::protocol::json_rpc::{create_error_response, JsonRpcMessage};
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::types::errors::{INTERNAL_ERROR, INVALID_PARAMS};

use super::{
    context::CallContext, discover, modern, params_object, prompts, CompletionRequest,
    HandlerResult, LogLevel, McpServer, FIELD_ARGUMENTS, FIELD_LEVEL, FIELD_META, FIELD_NAME,
    FIELD_PROGRESS_TOKEN, FIELD_URI,
};

impl McpServer {
    /// Handle one message with no way back to the client. See
    /// [`handle_message_with`](Self::handle_message_with) for one that has.
    pub async fn handle_message(
        &self,
        message: JsonRpcMessage,
        session_id: Option<&str>,
    ) -> HandlerResult {
        self.handle_message_with(message, session_id, CallContext::detached())
            .await
    }

    /// Handle one message, with a context a tool can speak through.
    pub async fn handle_message_with(
        &self,
        message: JsonRpcMessage,
        session_id: Option<&str>,
        context: CallContext,
    ) -> HandlerResult {
        // An answer to something this server asked is not a request to route:
        // it belongs to whoever is waiting for it.
        if message.is_response() || message.is_error_response() {
            if self.pending.resolve(&message) {
                return (None, None);
            }
            tracing::debug!("an answer arrived for a request this server never made");
            return (None, None);
        }

        // A custom method handler registered on the protocol handler takes
        // precedence over the built-in tool/resource dispatch, so servers can
        // override tools/list, tools/call, etc. (matching the Python semantics).
        if let Some(method) = message.method() {
            if self.protocol_handler.has_custom_handler(method) {
                return self
                    .protocol_handler
                    .handle_message(message, session_id)
                    .await;
            }
        }
        // A modern request declares its version on every call, and a server
        // that cannot speak it owes the client the list it can — a bare
        // rejection leaves nothing to renegotiate from.
        let modern = modern::is_modern_request(&message);
        if let Some((code, text, data)) = modern::reject_unsupported_version(&message) {
            if let Some(id) = message.id().cloned() {
                return (
                    Some(JsonRpcMessage::Error(create_error_response(
                        id, code, &text, data,
                    ))),
                    None,
                );
            }
        }

        let (mut response, session) = self.dispatch(message, session_id, context).await;
        if let Some(response) = response.as_mut() {
            modern::finish_response(response, modern);
        }
        (response, session)
    }

    /// Route one message to whatever answers it.
    async fn dispatch(
        &self,
        message: JsonRpcMessage,
        session_id: Option<&str>,
        context: CallContext,
    ) -> HandlerResult {
        let answered = match message.method() {
            Some(MessageMethod::SERVER_DISCOVER) => self.discover(&message),
            Some(MessageMethod::TOOLS_LIST) => self.answer(&message, self.tools.list_result()),
            Some(MessageMethod::TOOLS_CALL) => {
                return (self.call_tool(&message, context).await, None)
            }
            Some(MessageMethod::RESOURCES_LIST) => {
                self.answer(&message, self.resources.list_result())
            }
            Some(MessageMethod::RESOURCES_READ) => {
                return (self.read_resource(&message).await, None)
            }
            Some(MessageMethod::RESOURCES_TEMPLATES_LIST) => {
                self.answer(&message, self.resources.templates_list_result())
            }
            Some(MessageMethod::RESOURCES_SUBSCRIBE) => self.subscribe(&message, true),
            Some(MessageMethod::RESOURCES_UNSUBSCRIBE) => self.subscribe(&message, false),
            Some(MessageMethod::PROMPTS_LIST) => {
                self.answer(&message, prompts::list_result(&self.prompts))
            }
            Some(MessageMethod::PROMPTS_GET) => return (self.get_prompt(&message).await, None),
            Some(MessageMethod::LOGGING_SET_LEVEL) => self.set_log_level(&message),
            Some(MessageMethod::COMPLETION_COMPLETE) => {
                return (self.complete(&message).await, None)
            }
            _ => {
                return self
                    .protocol_handler
                    .handle_message(message, session_id)
                    .await
            }
        };
        (answered, None)
    }

    /// Answer a request with a result, or nothing when it had no id to answer.
    fn answer(&self, message: &JsonRpcMessage, result: Value) -> Option<JsonRpcMessage> {
        let id = message.id()?.clone();
        Some(self.protocol_handler.create_response(id, Some(result)))
    }

    fn fail(&self, message: &JsonRpcMessage, code: i64, text: &str) -> Option<JsonRpcMessage> {
        let id = message.id()?.clone();
        Some(self.protocol_handler.create_error_response(id, code, text))
    }

    /// Answer `server/discover`, which replaces `initialize` in the modern era
    /// and establishes nothing: it may be asked at any time, by anyone.
    fn discover(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let handler = &self.protocol_handler;
        self.answer(
            message,
            discover::discover_result(
                handler.server_info(),
                handler.capabilities(),
                self.instructions.as_deref(),
            ),
        )
    }

    async fn call_tool(
        &self,
        message: &JsonRpcMessage,
        context: CallContext,
    ) -> Option<JsonRpcMessage> {
        let params = params_object(message);
        let name = params
            .get(FIELD_NAME)
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = params.get(FIELD_ARGUMENTS).cloned().unwrap_or(json!({}));

        match self.tools.call(name, arguments, context).await {
            Some(result) => self.answer(message, result),
            // An unknown tool is an invalid call, not a tool that failed.
            None => self.fail(message, INVALID_PARAMS, &format!("Unknown tool: {name}")),
        }
    }

    async fn read_resource(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let uri = params_object(message)
            .get(FIELD_URI)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        match self.resources.read(&uri).await {
            Some(Ok(result)) => self.answer(message, result),
            Some(Err(error)) => {
                tracing::error!("Resource read error for {uri}: {error}");
                self.fail(
                    message,
                    INTERNAL_ERROR,
                    &format!("Resource read error: {error}"),
                )
            }
            None => self.fail(message, INVALID_PARAMS, &format!("Unknown resource: {uri}")),
        }
    }

    async fn get_prompt(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let params = params_object(message);
        let name = params
            .get(FIELD_NAME)
            .and_then(Value::as_str)
            .unwrap_or_default();
        let arguments = params
            .get(FIELD_ARGUMENTS)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();

        match prompts::get_result(&self.prompts, name, arguments).await {
            Ok(result) => self.answer(message, result),
            Err((code, text)) => self.fail(message, code, &text),
        }
    }

    /// `resources/subscribe` and its opposite. The client is asking to be told
    /// when a resource changes; recording the interest is the whole contract,
    /// and the answer is an empty result either way.
    fn subscribe(&self, message: &JsonRpcMessage, subscribe: bool) -> Option<JsonRpcMessage> {
        let uri = params_object(message)
            .get(FIELD_URI)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if uri.is_empty() {
            return self.fail(message, INVALID_PARAMS, "a subscription needs a uri");
        }
        if subscribe {
            self.subscriptions.add(&uri);
        } else {
            self.subscriptions.remove(&uri);
        }
        self.answer(message, json!({}))
    }

    /// `logging/setLevel`: remember how much the client wants to hear.
    fn set_log_level(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let params = params_object(message);
        let name = params
            .get(FIELD_LEVEL)
            .and_then(Value::as_str)
            .unwrap_or_default();

        match LogLevel::parse(name) {
            Some(level) => {
                self.log_level.set(level);
                self.answer(message, json!({}))
            }
            None => self.fail(
                message,
                INVALID_PARAMS,
                &format!("unknown log level: {name}"),
            ),
        }
    }

    async fn complete(&self, message: &JsonRpcMessage) -> Option<JsonRpcMessage> {
        let request = CompletionRequest::from_params(&params_object(message));
        let result = self.completions.complete(request).await;
        self.answer(message, result)
    }

    /// A context for handling `message`, sending anything it says to
    /// `outbound`.
    pub fn context_for(
        &self,
        message: &JsonRpcMessage,
        outbound: tokio::sync::mpsc::UnboundedSender<JsonRpcMessage>,
    ) -> CallContext {
        CallContext::new(
            outbound,
            self.pending.clone(),
            progress_token(message),
            self.client_timeout,
        )
    }
}

/// The progress token a request asked its progress be reported under.
fn progress_token(message: &JsonRpcMessage) -> Option<Value> {
    message
        .params()?
        .get(FIELD_META)?
        .get(FIELD_PROGRESS_TOKEN)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::json_rpc::parse_message;

    #[test]
    fn a_progress_token_is_read_from_the_meta_block() {
        let asked = parse_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_CALL,
            "params": {"_meta": {"progressToken": "p1"}}
        }))
        .unwrap();
        assert_eq!(progress_token(&asked), Some(json!("p1")));
    }

    /// Absent at any level means no token, rather than a token of `null` the
    /// client could not match to anything.
    #[test]
    fn a_request_without_one_has_no_progress_token() {
        let bare = parse_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_CALL, "params": {}
        }))
        .unwrap();
        assert_eq!(progress_token(&bare), None);

        let empty_meta = parse_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_CALL,
            "params": {"_meta": {}}
        }))
        .unwrap();
        assert_eq!(progress_token(&empty_meta), None);

        let no_params =
            parse_message(&json!({"jsonrpc": "2.0", "id": 1, "method": MessageMethod::PING}))
                .unwrap();
        assert_eq!(progress_token(&no_params), None);
    }
}
