//! Legacy SSE transport (deprecated as of MCP spec 2025-03-26), mirroring
//! `chuk_mcp.transports.sse`.
//!
//! Connects to `{base}/sse` for the event stream; the server announces a
//! message-POST endpoint via an `endpoint` event. Handles both immediate HTTP
//! responses (200) and async SSE-delivered responses (202).

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::{mpsc, Notify};

use crate::protocol::json_rpc::{parse_message_str, JsonRpcMessage};
use crate::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use crate::protocol::types::errors::{McpError, INTERNAL_ERROR};
use crate::transports::Transport;

/// Parameters for the (deprecated) SSE transport.
#[derive(Debug, Clone)]
pub struct SseParameters {
    /// Base URL for the SSE server (e.g. `http://localhost:3000`).
    pub url: String,
    /// Optional HTTP headers to send with requests.
    pub headers: HashMap<String, String>,
    /// Request timeout in seconds.
    pub timeout: f64,
    /// Optional bearer token (added to the Authorization header).
    pub bearer_token: Option<String>,
    /// SSE endpoint path (default `/sse`).
    pub sse_endpoint: String,
}

impl SseParameters {
    pub fn new(url: impl Into<String>) -> Result<Self, McpError> {
        let url: String = url.into();
        if url.is_empty() {
            return Err(McpError::validation("SSE URL cannot be empty"));
        }
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(McpError::validation(
                "SSE URL must start with http:// or https://",
            ));
        }
        Ok(SseParameters {
            url: url.trim_end_matches('/').to_string(),
            headers: HashMap::new(),
            timeout: 60.0,
            bearer_token: None,
            sse_endpoint: "/sse".to_string(),
        })
    }

    fn effective_headers(&self) -> HashMap<String, String> {
        let mut headers = self.headers.clone();
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

#[derive(Default)]
struct SseShared {
    message_url: std::sync::Mutex<Option<String>>,
    session_id: std::sync::Mutex<Option<String>>,
    connected: Notify,
}

/// Legacy SSE transport.
pub struct SseTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
    shared: Arc<SseShared>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl SseTransport {
    /// Connect to the SSE endpoint and start the reader and sender tasks.
    /// Waits (up to the configured timeout) for the server's `endpoint` event.
    pub async fn start(parameters: SseParameters) -> Result<Self, McpError> {
        let (incoming_tx, incoming) = message_channel(100);
        let (outgoing, outgoing_rx) = mpsc::channel::<JsonRpcMessage>(100);

        let shared = Arc::new(SseShared::default());

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs_f64(parameters.timeout))
            .build()
            .map_err(|e| McpError::Transport(format!("Failed to build HTTP client: {e}")))?;

        // Streaming client without a total-request timeout (the SSE stream is
        // long-lived); connection establishment is still bounded below.
        let stream_client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs_f64(
                parameters.timeout.min(15.0),
            ))
            .build()
            .map_err(|e| McpError::Transport(format!("Failed to build HTTP client: {e}")))?;

        let sse_task = tokio::spawn(handle_sse_connection(
            stream_client,
            parameters.clone(),
            shared.clone(),
            incoming_tx.clone(),
        ));

        let sender_task = tokio::spawn(outgoing_handler(
            client,
            parameters.clone(),
            shared.clone(),
            outgoing_rx,
            incoming_tx,
        ));

        let transport = SseTransport {
            incoming,
            outgoing,
            shared: shared.clone(),
            tasks: vec![sse_task, sender_task],
        };

        // Wait for the endpoint event before declaring the transport ready.
        let timeout = std::time::Duration::from_secs_f64(parameters.timeout);
        let connected = tokio::time::timeout(timeout, async {
            loop {
                if shared.message_url.lock().expect("url lock").is_some() {
                    return;
                }
                shared.connected.notified().await;
            }
        })
        .await;

        if connected.is_err() {
            return Err(McpError::Transport(format!(
                "Timeout waiting for SSE connection to {}",
                parameters.url
            )));
        }

        tracing::info!("SSE connection established to {}", parameters.url);
        Ok(transport)
    }

    /// Whether the endpoint event has been received.
    pub fn is_connected(&self) -> bool {
        self.shared.message_url.lock().expect("url lock").is_some()
    }

    /// The session id extracted from the endpoint event, if any.
    pub fn get_session_id(&self) -> Option<String> {
        self.shared.session_id.lock().expect("session lock").clone()
    }
}

/// Long-lived task reading the SSE event stream.
async fn handle_sse_connection(
    client: reqwest::Client,
    params: SseParameters,
    shared: Arc<SseShared>,
    incoming_tx: mpsc::Sender<JsonRpcMessage>,
) {
    let sse_url = format!("{}{}", params.url, params.sse_endpoint);
    tracing::info!("Connecting to SSE endpoint: {sse_url}");

    let mut request = client
        .get(&sse_url)
        .header("Accept", "text/event-stream")
        .header("Cache-Control", "no-cache");
    for (key, value) in params.effective_headers() {
        request = request.header(key, value);
    }

    let response = match request.send().await {
        Ok(r) if r.status().is_success() => r,
        Ok(r) => {
            tracing::error!("SSE connection failed with status {}", r.status());
            return;
        }
        Err(e) => {
            tracing::error!("SSE connection error: {e}");
            return;
        }
    };

    let mut buffer = String::new();
    let mut stream = response.bytes_stream();
    let mut current_event: Option<String> = None;

    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline) = buffer.find('\n') {
            let line: String = buffer.drain(..=newline).collect();
            let line = line.trim_end_matches(['\n', '\r']);

            if line.is_empty() {
                current_event = None;
                continue;
            }

            if let Some(rest) = line.strip_prefix("event:") {
                current_event = Some(rest.trim().to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                let data = rest.trim();
                match current_event.as_deref() {
                    Some("endpoint") => handle_endpoint_event(data, &params, &shared),
                    Some("message") => route_message_data(data, &incoming_tx).await,
                    Some("keepalive") => tracing::debug!("Received keepalive"),
                    _ => {
                        // Untyped data: endpoint announcement or JSON-RPC.
                        let no_url = shared.message_url.lock().expect("url lock").is_none();
                        if no_url && (data.contains("/messages/") || data.contains("/mcp")) {
                            handle_endpoint_event(data, &params, &shared);
                        } else if data.starts_with('{') && data.contains("\"jsonrpc\"") {
                            route_message_data(data, &incoming_tx).await;
                        } else {
                            tracing::debug!("Unknown SSE data: {:.100}", data);
                        }
                    }
                }
            }
        }
    }
    tracing::debug!("SSE stream ended");
}

/// Handle the `endpoint` event announcing where to POST messages.
fn handle_endpoint_event(data: &str, params: &SseParameters, shared: &SseShared) {
    let endpoint = data.trim();
    let message_url = if endpoint.starts_with('/') {
        format!("{}{endpoint}", params.url)
    } else if endpoint.starts_with("http") {
        endpoint.to_string()
    } else if endpoint.contains('=') {
        // Query parameters only.
        format!("{}/messages/?{endpoint}", params.url)
    } else {
        endpoint.to_string()
    };

    if let Some(session) = message_url
        .split("session_id=")
        .nth(1)
        .map(|s| s.split('&').next().unwrap_or(s).to_string())
    {
        tracing::info!("Session ID: {session}");
        *shared.session_id.lock().expect("session lock") = Some(session);
    }

    tracing::info!("Message URL set to: {message_url}");
    *shared.message_url.lock().expect("url lock") = Some(message_url);
    shared.connected.notify_waiters();
}

/// Parse SSE-delivered JSON-RPC data and route it to the incoming stream.
async fn route_message_data(data: &str, incoming_tx: &mpsc::Sender<JsonRpcMessage>) {
    match parse_message_str(data) {
        Ok(msg) => {
            let _ = incoming_tx.send(msg).await;
        }
        Err(e) => tracing::error!("Failed to parse SSE message JSON: {e}"),
    }
}

/// Task POSTing outgoing messages to the announced message endpoint.
async fn outgoing_handler(
    client: reqwest::Client,
    params: SseParameters,
    shared: Arc<SseShared>,
    mut outgoing_rx: mpsc::Receiver<JsonRpcMessage>,
    incoming_tx: mpsc::Sender<JsonRpcMessage>,
) {
    while let Some(message) = outgoing_rx.recv().await {
        let Some(message_url) = shared.message_url.lock().expect("url lock").clone() else {
            tracing::error!("Cannot send message: message URL not available");
            continue;
        };

        let message_id = message.id().cloned();
        let mut request = client
            .post(&message_url)
            .header("Content-Type", "application/json");
        for (key, value) in params.effective_headers() {
            request = request.header(key, value);
        }

        match request.json(&message.to_value()).send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                if status == 200 {
                    // Immediate HTTP response: route the body.
                    if let Ok(text) = response.text().await {
                        if !text.is_empty() {
                            route_message_data(&text, &incoming_tx).await;
                        }
                    }
                } else if status == 202 {
                    // Async: the response arrives via the SSE stream and is
                    // routed by the reader task.
                    tracing::debug!("Message {message_id:?} accepted, awaiting SSE response");
                } else {
                    let text = response.text().await.unwrap_or_default();
                    // Try to parse the body anyway; otherwise synthesize error.
                    if parse_message_str(&text).is_ok() {
                        route_message_data(&text, &incoming_tx).await;
                    } else if let Some(id) = message_id {
                        let _ = incoming_tx
                            .send(JsonRpcMessage::Error(
                                crate::protocol::json_rpc::create_error_response(
                                    id,
                                    INTERNAL_ERROR,
                                    &format!("HTTP {status}: {:.100}", text),
                                    None,
                                ),
                            ))
                            .await;
                    }
                }
            }
            Err(e) => {
                tracing::error!("Error sending request: {e}");
                if let Some(id) = message_id {
                    let _ = incoming_tx
                        .send(JsonRpcMessage::Error(
                            crate::protocol::json_rpc::create_error_response(
                                id,
                                INTERNAL_ERROR,
                                &e.to_string(),
                                None,
                            ),
                        ))
                        .await;
                }
            }
        }
    }
}

#[async_trait]
impl Transport for SseTransport {
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), McpError> {
        Ok((self.incoming.clone(), self.outgoing.clone()))
    }

    async fn close(&mut self) -> Result<(), McpError> {
        for task in self.tasks.drain(..) {
            task.abort();
        }
        Ok(())
    }
}

impl Drop for SseTransport {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_validation() {
        assert!(SseParameters::new("").is_err());
        assert!(SseParameters::new("ws://x").is_err());
        let p = SseParameters::new("http://localhost:3000/").unwrap();
        assert_eq!(p.url, "http://localhost:3000");
    }

    #[test]
    fn endpoint_event_parsing() {
        let params = SseParameters::new("http://localhost:3000").unwrap();
        let shared = SseShared::default();

        handle_endpoint_event("/messages/?session_id=abc123", &params, &shared);
        assert_eq!(
            shared.message_url.lock().unwrap().as_deref(),
            Some("http://localhost:3000/messages/?session_id=abc123")
        );
        assert_eq!(shared.session_id.lock().unwrap().as_deref(), Some("abc123"));
    }
}
