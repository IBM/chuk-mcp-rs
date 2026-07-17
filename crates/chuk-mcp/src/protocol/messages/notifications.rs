//! Notification handling, mirroring `chuk_mcp.protocol.messages.notifications`.

use serde_json::{json, Value};

use crate::protocol::json_rpc::{create_notification, JsonRpcMessage, RequestId};
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::WriteStream;
use crate::protocol::types::errors::McpError;

/// Send a progress notification for a long-running operation.
pub async fn send_progress_notification(
    write_stream: &WriteStream,
    progress_token: RequestId,
    progress: f64,
    total: Option<f64>,
    message: Option<&str>,
) -> Result<(), McpError> {
    let mut params = json!({
        "progressToken": progress_token,
        "progress": progress,
    });
    if let Some(total) = total {
        params["total"] = json!(total);
    }
    if let Some(message) = message {
        params["message"] = json!(message);
    }

    send_notification(
        write_stream,
        MessageMethod::NOTIFICATION_PROGRESS,
        Some(params),
    )
    .await
}

/// Send a cancellation notification for a previously-issued request.
pub async fn send_cancelled_notification(
    write_stream: &WriteStream,
    request_id: RequestId,
    reason: Option<&str>,
) -> Result<(), McpError> {
    let mut params = json!({"requestId": request_id});
    if let Some(reason) = reason {
        params["reason"] = json!(reason);
    }
    send_notification(
        write_stream,
        MessageMethod::NOTIFICATION_CANCELLED,
        Some(params),
    )
    .await
}

/// Send a notification that the roots list has changed.
pub async fn send_roots_list_changed_notification(
    write_stream: &WriteStream,
) -> Result<(), McpError> {
    send_notification(
        write_stream,
        MessageMethod::NOTIFICATION_ROOTS_LIST_CHANGED,
        Some(json!({})),
    )
    .await
}

/// Send an arbitrary notification.
pub async fn send_notification(
    write_stream: &WriteStream,
    method: &str,
    params: Option<Value>,
) -> Result<(), McpError> {
    write_stream
        .send(JsonRpcMessage::Notification(create_notification(
            method, params,
        )))
        .await
        .map_err(|_| McpError::Transport("write stream closed".into()))
}

/// Parsed fields of a `notifications/progress` notification.
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressNotification {
    pub progress_token: Option<RequestId>,
    pub progress: f64,
    pub total: Option<f64>,
    pub message: Option<String>,
}

/// Extract progress fields if the message is a progress notification.
pub fn parse_progress_notification(msg: &JsonRpcMessage) -> Option<ProgressNotification> {
    if msg.method() != Some(MessageMethod::NOTIFICATION_PROGRESS) {
        return None;
    }
    let params = msg.params()?;
    Some(ProgressNotification {
        progress_token: params
            .get("progressToken")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        progress: params
            .get("progress")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        total: params.get("total").and_then(Value::as_f64),
        message: params
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Parsed fields of a `notifications/cancelled` notification.
#[derive(Debug, Clone, PartialEq)]
pub struct CancelledNotification {
    pub request_id: Option<RequestId>,
    pub reason: Option<String>,
}

/// Extract fields if the message is a cancellation notification.
pub fn parse_cancelled_notification(msg: &JsonRpcMessage) -> Option<CancelledNotification> {
    if msg.method() != Some(MessageMethod::NOTIFICATION_CANCELLED) {
        return None;
    }
    let params = msg.params()?;
    Some(CancelledNotification {
        request_id: params
            .get("requestId")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        reason: params
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Parsed fields of a `notifications/message` (server logging) notification.
#[derive(Debug, Clone, PartialEq)]
pub struct LoggingMessageNotification {
    pub level: String,
    pub data: Option<Value>,
    pub logger: Option<String>,
}

/// Extract fields if the message is a server logging notification.
pub fn parse_logging_message_notification(
    msg: &JsonRpcMessage,
) -> Option<LoggingMessageNotification> {
    if msg.method() != Some(MessageMethod::NOTIFICATION_MESSAGE) {
        return None;
    }
    let params = msg.params()?;
    Some(LoggingMessageNotification {
        level: params
            .get("level")
            .and_then(Value::as_str)
            .unwrap_or("info")
            .to_string(),
        data: params.get("data").cloned(),
        logger: params
            .get("logger")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

/// Handler callback for a notification, given its full JSON-RPC message.
pub type NotificationCallback = Box<dyn Fn(&JsonRpcMessage) + Send + Sync>;

/// Centralized notification router: register callbacks per method.
#[derive(Default)]
pub struct NotificationHandler {
    handlers: std::collections::HashMap<String, NotificationCallback>,
}

impl NotificationHandler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler for a specific notification method.
    pub fn register(&mut self, method: &str, handler: NotificationCallback) {
        self.handlers.insert(method.to_string(), handler);
    }

    /// Route a notification to the appropriate handler, if any.
    pub fn handle(&self, notification: &JsonRpcMessage) {
        let Some(method) = notification.method() else {
            tracing::warn!("Received notification without method");
            return;
        };
        match self.handlers.get(method) {
            Some(handler) => handler(notification),
            None => tracing::debug!("No handler registered for {method}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_progress() {
        let msg = JsonRpcMessage::Notification(create_notification(
            MessageMethod::NOTIFICATION_PROGRESS,
            Some(json!({"progressToken": "t", "progress": 0.25, "total": 1.0})),
        ));
        let parsed = parse_progress_notification(&msg).unwrap();
        assert_eq!(parsed.progress, 0.25);
        assert_eq!(parsed.progress_token, Some("t".into()));
    }

    #[test]
    fn router_dispatches() {
        let mut handler = NotificationHandler::new();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let hits2 = hits.clone();
        handler.register(
            MessageMethod::NOTIFICATION_INITIALIZED,
            Box::new(move |_| {
                hits2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );

        let msg = JsonRpcMessage::Notification(create_notification(
            MessageMethod::NOTIFICATION_INITIALIZED,
            None,
        ));
        handler.handle(&msg);
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
