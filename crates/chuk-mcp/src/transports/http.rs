//! Streamable HTTP transport (MCP spec 2025-03-26), mirroring
//! `chuk_mcp.transports.http`.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Semaphore};

use crate::protocol::json_rpc::{parse_message_str, JsonRpcMessage};
use crate::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use crate::protocol::types::errors::{McpError, INTERNAL_ERROR, PARSE_ERROR};
use crate::transports::limits::{
    exceeds_limit, read_body_bounded, too_large_error, TransportLimits,
};
use crate::transports::Transport;

/// Parameters for Streamable HTTP transport.
#[derive(Debug, Clone)]
pub struct StreamableHttpParameters {
    /// Base URL for the MCP server (e.g. `http://localhost:3000/mcp`).
    pub url: String,
    /// Optional HTTP headers to send with requests.
    pub headers: HashMap<String, String>,
    /// Request timeout in seconds.
    pub timeout: f64,
    /// Optional bearer token (added to the Authorization header).
    pub bearer_token: Option<String>,
    /// Optional session id for reconnecting to existing sessions.
    pub session_id: Option<String>,
    /// User agent string for HTTP requests.
    pub user_agent: String,
    /// Whether to accept SSE streaming responses when available.
    pub enable_streaming: bool,
    /// Maximum number of concurrent requests.
    pub max_concurrent_requests: usize,
}

impl StreamableHttpParameters {
    pub fn new(url: impl Into<String>) -> Result<Self, McpError> {
        let url: String = url.into();
        if url.is_empty() {
            return Err(McpError::validation("Streamable HTTP URL cannot be empty"));
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(McpError::validation(
                "Streamable HTTP URL must start with http:// or https://",
            ));
        }
        Ok(StreamableHttpParameters {
            url: url.trim_end_matches('/').to_string(),
            headers: HashMap::new(),
            timeout: 60.0,
            bearer_token: None,
            session_id: None,
            user_agent: "chuk-mcp/1.0.0".to_string(),
            enable_streaming: true,
            max_concurrent_requests: 10,
        })
    }

    pub fn with_bearer_token(mut self, token: impl Into<String>) -> Self {
        self.bearer_token = Some(token.into());
        self
    }

    pub fn with_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.headers = headers;
        self
    }

    /// The effective headers: user's, plus User-Agent and Authorization.
    fn effective_headers(&self) -> HashMap<String, String> {
        let mut headers = self.headers.clone();
        if !headers.keys().any(|k| k.eq_ignore_ascii_case("user-agent")) {
            headers.insert("User-Agent".to_string(), self.user_agent.clone());
        }
        if let Some(token) = &self.bearer_token {
            if !headers
                .keys()
                .any(|k| k.eq_ignore_ascii_case("authorization"))
            {
                let value = if token.starts_with("Bearer ") {
                    token.clone()
                } else {
                    format!("Bearer {token}")
                };
                headers.insert("Authorization".to_string(), value);
            }
        }
        headers
    }
}

/// Streamable HTTP transport: POSTs each message; handles both immediate JSON
/// responses and SSE-streamed responses.
pub struct StreamableHttpTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
    session_id: Arc<std::sync::Mutex<Option<String>>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl StreamableHttpTransport {
    /// Start the transport and its outgoing message handler.
    pub fn start(parameters: StreamableHttpParameters) -> Result<Self, McpError> {
        Self::start_with_limits(parameters, TransportLimits::default())
    }

    /// Start the transport with explicit buffer limits.
    pub fn start_with_limits(
        parameters: StreamableHttpParameters,
        limits: TransportLimits,
    ) -> Result<Self, McpError> {
        let (incoming_tx, incoming) = message_channel(100);
        let (outgoing, mut outgoing_rx) = mpsc::channel::<JsonRpcMessage>(100);

        let session_id = Arc::new(std::sync::Mutex::new(parameters.session_id.clone()));
        let session_for_task = session_id.clone();

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs_f64(parameters.timeout))
            .build()
            .map_err(|e| McpError::Transport(format!("Failed to build HTTP client: {e}")))?;

        let semaphore = Arc::new(Semaphore::new(parameters.max_concurrent_requests.max(1)));
        let max_buffer_size = limits.max_buffer_size;

        let task = tokio::spawn(async move {
            while let Some(message) = outgoing_rx.recv().await {
                let permit = semaphore.clone().acquire_owned().await;
                let client = client.clone();
                let params = parameters.clone();
                let incoming_tx = incoming_tx.clone();
                let session = session_for_task.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    send_via_http(
                        &client,
                        &params,
                        &session,
                        &incoming_tx,
                        message,
                        max_buffer_size,
                    )
                    .await;
                });
            }
        });

        Ok(StreamableHttpTransport {
            incoming,
            outgoing,
            session_id,
            task: Some(task),
        })
    }

    /// The current MCP session id, if the server assigned one.
    pub fn get_session_id(&self) -> Option<String> {
        self.session_id.lock().expect("session lock").clone()
    }
}

/// POST one message and route its response(s) to the incoming stream.
async fn send_via_http(
    client: &reqwest::Client,
    params: &StreamableHttpParameters,
    session: &Arc<std::sync::Mutex<Option<String>>>,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    message: JsonRpcMessage,
    max_buffer_size: usize,
) {
    let message_id = message.id().cloned();
    let method = message.method().unwrap_or("unknown").to_string();
    tracing::debug!("Sending HTTP message: {method} (id: {message_id:?})");

    let mut request = client
        .post(&params.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");

    for (key, value) in params.effective_headers() {
        if !matches!(key.as_str(), "Content-Type" | "Accept") {
            request = request.header(key, value);
        }
    }

    if let Some(session_id) = session.lock().expect("session lock").clone() {
        request = request.header("Mcp-Session-Id", session_id);
    }

    let response = match request.json(&message.to_value()).send().await {
        Ok(response) => response,
        Err(e) => {
            route_error(incoming_tx, &message_id, INTERNAL_ERROR, &e.to_string()).await;
            return;
        }
    };

    // Capture session id assigned by the server.
    if let Some(sid) = response
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        *session.lock().expect("session lock") = Some(sid.to_string());
    }

    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if status.as_u16() >= 400 {
        let text = read_body_bounded(response, max_buffer_size, "HTTP response")
            .await
            .unwrap_or_default();
        route_error(
            incoming_tx,
            &message_id,
            INTERNAL_ERROR,
            &format!("HTTP {status}: {text}"),
        )
        .await;
        return;
    }

    if content_type.contains("text/event-stream") {
        stream_sse_response(response, incoming_tx, max_buffer_size).await;
        return;
    }

    let text = match read_body_bounded(response, max_buffer_size, "HTTP response").await {
        Ok(text) => text,
        Err(e) => {
            route_error(incoming_tx, &message_id, INTERNAL_ERROR, &e.to_string()).await;
            return;
        }
    };
    if text.is_empty() {
        // Empty body (e.g. 202 Accepted). Fine for notifications; synthesize
        // an empty success for requests so callers don't hang.
        if let Some(id) = message_id {
            let _ = incoming_tx
                .send(JsonRpcMessage::Response(
                    crate::protocol::json_rpc::create_response(id, None),
                ))
                .await;
        }
        return;
    }

    // JSON (or JSON-looking) body; also tolerate SSE-formatted bodies.
    if text.starts_with("event:") || text.starts_with("data:") {
        parse_sse_text(&text, incoming_tx).await;
    } else {
        match parse_message_str(&text) {
            Ok(msg) => {
                let _ = incoming_tx.send(msg).await;
            }
            Err(e) => {
                route_error(
                    incoming_tx,
                    &message_id,
                    PARSE_ERROR,
                    &format!("Parse error: {e}"),
                )
                .await
            }
        }
    }
}

/// Route a synthesized error response for a request id.
async fn route_error(
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    message_id: &Option<crate::protocol::json_rpc::RequestId>,
    code: i64,
    message: &str,
) {
    tracing::debug!("HTTP transport error for {message_id:?}: {message}");
    if let Some(id) = message_id {
        let _ = incoming_tx
            .send(JsonRpcMessage::Error(
                crate::protocol::json_rpc::create_error_response(id.clone(), code, message, None),
            ))
            .await;
    }
}

/// Stream and parse an SSE response body, routing JSON-RPC messages.
async fn stream_sse_response(
    response: reqwest::Response,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    max_buffer_size: usize,
) {
    use futures::StreamExt;

    let mut buffer = String::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // A server that never completes an event would otherwise grow this
        // buffer without bound - abort instead of exhausting memory.
        if exceeds_limit(buffer.len(), max_buffer_size) {
            tracing::error!(
                "{}",
                too_large_error(buffer.len(), max_buffer_size, "SSE event")
            );
            return;
        }

        // Process complete events (separated by blank lines).
        while let Some(pos) = find_event_boundary(&buffer) {
            let event_text: String = buffer.drain(..pos).collect();
            process_sse_event_text(&event_text, incoming_tx).await;
        }
    }
    // Trailing event without final blank line.
    if !buffer.trim().is_empty() {
        process_sse_event_text(&buffer, incoming_tx).await;
    }
}

/// Find the end of the first complete SSE event (blank-line separator),
/// returning the index just past the separator.
fn find_event_boundary(buffer: &str) -> Option<usize> {
    let lf = buffer.find("\n\n").map(|i| i + 2);
    let crlf = buffer.find("\r\n\r\n").map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Parse a fully-loaded SSE body.
async fn parse_sse_text(text: &str, incoming_tx: &mpsc::Sender<JsonRpcMessage>) {
    for event_text in text.split("\n\n") {
        process_sse_event_text(event_text, incoming_tx).await;
    }
}

/// Process one SSE event's raw text: extract `data:` lines and route
/// message-bearing events.
async fn process_sse_event_text(event_text: &str, incoming_tx: &mpsc::Sender<JsonRpcMessage>) {
    let mut event_type: Option<&str> = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in event_text.lines() {
        let line = line.trim_end_matches('\r');
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }

    if data_lines.is_empty() {
        return;
    }
    // Message-bearing events: explicit message/response types or untyped data.
    if matches!(event_type, Some("message") | Some("response") | None) {
        let full_data = data_lines.join("\n");
        if full_data.trim_start().starts_with('{') {
            match parse_message_str(full_data.trim()) {
                Ok(msg) => {
                    let _ = incoming_tx.send(msg).await;
                }
                Err(e) => tracing::error!("Failed to parse SSE message JSON: {e}"),
            }
        }
    }
}

#[async_trait]
impl Transport for StreamableHttpTransport {
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), McpError> {
        Ok((self.incoming.clone(), self.outgoing.clone()))
    }

    async fn close(&mut self) -> Result<(), McpError> {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        Ok(())
    }
}

impl Drop for StreamableHttpTransport {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation() {
        assert!(StreamableHttpParameters::new("").is_err());
        assert!(StreamableHttpParameters::new("ftp://x").is_err());
        let p = StreamableHttpParameters::new("http://localhost:3000/mcp/").unwrap();
        assert_eq!(p.url, "http://localhost:3000/mcp");
    }

    #[test]
    fn bearer_token_header() {
        let p = StreamableHttpParameters::new("http://x")
            .unwrap()
            .with_bearer_token("abc");
        assert_eq!(
            p.effective_headers().get("Authorization").unwrap(),
            "Bearer abc"
        );

        let p = StreamableHttpParameters::new("http://x")
            .unwrap()
            .with_bearer_token("Bearer xyz");
        assert_eq!(
            p.effective_headers().get("Authorization").unwrap(),
            "Bearer xyz"
        );
    }

    #[test]
    fn event_boundary() {
        assert_eq!(find_event_boundary("data: x\n\nrest"), Some(9));
        assert_eq!(find_event_boundary("data: x"), None);
    }
}
