//! Stateless Streamable HTTP transport (MCP `2026-07-28`).
//!
//! Sits alongside [`crate::transports::http`] rather than replacing it: that one
//! implements the legacy stateful shape and stays frozen until its deprecation
//! window closes. This one implements the modern shape, which differs in ways
//! that are not expressible as options on the old transport:
//!
//! * **No protocol sessions.** No `Mcp-Session-Id` is sent, and one arriving in
//!   a response is ignored rather than stored.
//! * **Per-request metadata.** Every request carries its protocol version,
//!   client capabilities and identity in `_meta`, mirrored into headers. The
//!   transport injects them, so no caller can forget and earn a `-32602`.
//! * **No stream resumability.** `Last-Event-ID` is gone. If a response stream
//!   dies before delivering its response, the request is lost and **MUST** be
//!   re-issued as a *new* request with a *new* id.
//!
//! That last rule is the interesting one. The caller is waiting on the id it
//! wrote, so re-issuing under a fresh wire id would strand it. This transport
//! keeps the mapping internally: it retries with a new id and rewrites the
//! eventual response back to the caller's id. Retrying is the driver's job, not
//! the caller's.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::{mpsc, Semaphore};

use crate::protocol::envelope::{self, ClientIdentity, Envelope};
use crate::protocol::era::{self, Detection};
use crate::protocol::json_rpc::{
    create_error_response, create_notification, create_request, create_response, parse_message_str,
    JsonRpcMessage, RequestId,
};
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use crate::protocol::tool_schemas::ToolSchemas;
use crate::protocol::types::errors::{
    McpError, LOCAL_MALFORMED_RESPONSE, LOCAL_REQUEST_REJECTED, LOCAL_STREAM_LOST,
    LOCAL_TRANSPORT_FAILURE,
};
use crate::protocol::versioning;
use crate::transports::limits::{read_body_bounded, TransportLimits};
use crate::transports::Transport;

/// Parameters for the stateless Streamable HTTP transport.
///
/// Deliberately has no `session_id`: there are no protocol sessions to rejoin.
#[derive(Debug, Clone)]
pub struct ModernHttpParameters {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub timeout: f64,
    pub bearer_token: Option<String>,
    /// The authorization state for this endpoint, when OAuth is in play.
    ///
    /// Shared rather than owned: every request on this connection reads the
    /// same token, and a `401` on one of them re-authorizes for all.
    #[cfg(feature = "auth")]
    pub auth: Option<Arc<crate::auth::AuthSession>>,
    pub user_agent: String,
    pub max_concurrent_requests: usize,
    /// Sent in every request's `_meta`.
    pub identity: ClientIdentity,
    /// Declared on every request, in `_meta` and the `MCP-Protocol-Version` header.
    pub protocol_version: String,
    /// How many times to re-issue a request whose response stream died.
    pub max_stream_retries: usize,
}

impl ModernHttpParameters {
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
        Ok(ModernHttpParameters {
            url: url.trim_end_matches('/').to_string(),
            headers: HashMap::new(),
            timeout: 60.0,
            bearer_token: None,
            #[cfg(feature = "auth")]
            auth: None,
            user_agent: concat!("chuk-mcp/", env!("CARGO_PKG_VERSION")).to_string(),
            max_concurrent_requests: 10,
            identity: ClientIdentity::chuk(),
            protocol_version: versioning::FIRST_MODERN_VERSION.to_string(),
            max_stream_retries: 2,
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

    pub fn with_identity(mut self, identity: ClientIdentity) -> Self {
        self.identity = identity;
        self
    }

    pub fn with_max_stream_retries(mut self, retries: usize) -> Self {
        self.max_stream_retries = retries;
        self
    }

    /// User headers plus User-Agent and Authorization.
    ///
    /// Any `Mcp-Session-Id` a caller supplies is dropped: it has no meaning in
    /// this revision, and forwarding it could make a dual-era server select
    /// legacy semantics for a request that is otherwise modern.
    fn effective_headers(&self) -> HashMap<String, String> {
        let mut headers: HashMap<String, String> = self
            .headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case("Mcp-Session-Id"))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        headers.insert("User-Agent".to_string(), self.user_agent.clone());

        // A token obtained by the authorization flow wins: it is the current
        // one, while `bearer_token` is whatever the caller configured before
        // any flow ran.
        #[cfg(feature = "auth")]
        let token = self
            .auth
            .as_ref()
            .and_then(|session| session.bearer())
            .or_else(|| self.bearer_token.clone());
        #[cfg(not(feature = "auth"))]
        let token = self.bearer_token.clone();

        if let Some(token) = token {
            headers.insert("Authorization".to_string(), format!("Bearer {token}"));
        }
        headers
    }
}

/// The stateless Streamable HTTP transport.
pub struct ModernHttpTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ModernHttpTransport {
    pub fn start(parameters: ModernHttpParameters) -> Result<Self, McpError> {
        Self::start_with_limits(parameters, TransportLimits::default())
    }

    /// Start the transport with explicit buffer limits.
    pub fn start_with_limits(
        parameters: ModernHttpParameters,
        limits: TransportLimits,
    ) -> Result<Self, McpError> {
        let (incoming_tx, incoming) = message_channel(100);
        let (outgoing, mut outgoing_rx) = mpsc::channel::<JsonRpcMessage>(100);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs_f64(parameters.timeout))
            .build()
            .map_err(|e| McpError::Transport(format!("Failed to build HTTP client: {e}")))?;

        let semaphore = Arc::new(Semaphore::new(parameters.max_concurrent_requests.max(1)));
        let max_buffer_size = limits.max_buffer_size;
        // Filled as `tools/list` responses go by, read when a `tools/call`
        // needs to know which of its parameters to mirror into headers.
        let schemas = ToolSchemas::new();

        let task = tokio::spawn(async move {
            while let Some(message) = outgoing_rx.recv().await {
                let permit = semaphore.clone().acquire_owned().await;
                let client = client.clone();
                let params = parameters.clone();
                let incoming_tx = incoming_tx.clone();
                let schemas = schemas.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    dispatch_modern(
                        &client,
                        &params,
                        &incoming_tx,
                        message,
                        false,
                        max_buffer_size,
                        &schemas,
                    )
                    .await;
                });
            }
        });

        Ok(ModernHttpTransport {
            incoming,
            outgoing,
            task: Some(task),
        })
    }
}

/// What one HTTP attempt concluded.
#[derive(Debug, PartialEq, Eq)]
enum Attempt {
    /// A final response or error reached the caller.
    Completed,
    /// The stream ended with no response. The request is lost; re-issue it.
    StreamBroken,
    /// A terminal condition was already reported to the caller. The carried
    /// [`Detection`] records what, if anything, the failure proved about the
    /// peer's era — a `401` or `5xx` proves nothing either way.
    Reported(Detection),
    /// The response identifies the peer as *not* modern. Nothing was routed to
    /// the caller, so the request can be re-sent under the legacy lifecycle.
    NotModern,
    /// The peer demanded authorization and a token was obtained. Nothing was
    /// routed to the caller, so the request can be sent again — this time with
    /// a credential on it.
    #[cfg(feature = "auth")]
    Reauthorized,
}

/// How a dispatch ended, from a dual-era caller's point of view.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Dispatched {
    /// The caller has been answered, and the peer positively identified itself
    /// as modern — either by succeeding or by returning a recognised modern
    /// error. Safe to cache.
    HandledModern,
    /// The caller has been answered, but nothing was learned about the era: a
    /// timeout, an auth failure, a `5xx`. **Must not** be cached, or a
    /// momentarily unhealthy server would be pinned to an era it never claimed.
    HandledUndetermined,
    /// The peer is legacy and nothing was routed. Re-send this request through
    /// the legacy transport.
    ///
    /// Re-sending is safe: every response that yields this verdict — a `400`
    /// without a modern error, a bare `404`, a `405` — means the request was
    /// rejected before being processed, so there is no risk of executing it
    /// twice.
    FallBackToLegacy,
}

/// Send one outgoing message, retrying a broken response stream under a new id.
///
/// `allow_fallback` is set only while the peer's era is still unknown. Once an
/// endpoint is known to be modern, a `400` is a real error and must reach the
/// caller rather than silently triggering a legacy retry.
pub(crate) async fn dispatch_modern(
    client: &reqwest::Client,
    params: &ModernHttpParameters,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    message: JsonRpcMessage,
    allow_fallback: bool,
    max_buffer_size: usize,
    schemas: &ToolSchemas,
) -> Dispatched {
    let caller_id = message.id().cloned();
    let Some(method) = message.method().map(str::to_string) else {
        // Only requests and notifications go out; a client must not send
        // responses, and a batch has no single method to mirror into headers.
        route_error(
            incoming_tx,
            &caller_id,
            LOCAL_REQUEST_REJECTED,
            "modern Streamable HTTP sends one request or notification per POST",
        )
        .await;
        return Dispatched::HandledUndetermined;
    };

    let envelope = match envelope::build_envelope(
        &method,
        message.params().cloned(),
        &params.protocol_version,
        &params.identity,
    ) {
        Ok(envelope) => envelope,
        Err(e) => {
            // e.g. tools/call without a name: rejected here rather than
            // becoming a -32020 round trip.
            route_error(
                incoming_tx,
                &caller_id,
                LOCAL_REQUEST_REJECTED,
                &e.to_string(),
            )
            .await;
            return Dispatched::HandledUndetermined;
        }
    };

    // Mirror any `x-mcp-header` parameters this tool asked for. The schema
    // comes from a `tools/list` this connection has already seen; a tool that
    // was never listed simply has nothing to promote, which is not an error.
    //
    // A schema that violates the annotation constraints costs this one call
    // rather than the connection: `list_tools` already excludes such tools, so
    // reaching here means the caller named it without listing it.
    let mut envelope = envelope;
    if envelope.method == MessageMethod::TOOLS_CALL {
        if let Some(schema) = envelope
            .params
            .get("name")
            .and_then(Value::as_str)
            .and_then(|name| schemas.get(name))
        {
            let arguments = envelope
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            if let Err(e) = envelope.promote_tool_params(&schema, &arguments) {
                tracing::warn!("not promoting parameters for this call: {e}");
            }
        }
    }
    let envelope = envelope;

    debug_assert!(envelope.headers_match_body());
    debug_assert!(!envelope.has_session_header());

    let mut attempt = 0usize;
    loop {
        // First try uses the caller's id; every re-issue gets a fresh one,
        // because a broken stream loses the request and the spec forbids
        // reusing its id.
        let wire_id = match (&caller_id, attempt) {
            (None, _) => None,
            (Some(id), 0) => Some(id.clone()),
            (Some(_), _) => Some(RequestId::Str(uuid::Uuid::new_v4().to_string())),
        };
        // `attempt` counts *stream* re-issues, which need a fresh id. An
        // authorization retry does not touch it: the server never processed
        // the request, so re-sending it under the same id is correct.

        let ctx = AttemptCtx {
            schemas,
            envelope: &envelope,
            wire_id: &wire_id,
            caller_id: &caller_id,
            allow_fallback,
            max_buffer_size,
        };
        match send_once(client, params, incoming_tx, &ctx).await {
            Attempt::NotModern => return Dispatched::FallBackToLegacy,
            // A stream only exists after a 2xx, so the peer is modern either way.
            Attempt::StreamBroken if attempt < params.max_stream_retries => {
                tracing::debug!(
                    "response stream for {caller_id:?} ended without a response; \
                     re-issuing as a new request (attempt {})",
                    attempt + 2
                );
                attempt += 1;
            }
            Attempt::StreamBroken => {
                route_error(
                    incoming_tx,
                    &caller_id,
                    LOCAL_STREAM_LOST,
                    "response stream ended before a response arrived, and re-issuing \
                     the request did not succeed",
                )
                .await;
                return Dispatched::HandledModern;
            }
            // Re-sent with the token the challenge provoked. The id is reused
            // deliberately: the server refused this request before acting on
            // it, so it is the same request, not a new one.
            #[cfg(feature = "auth")]
            Attempt::Reauthorized => continue,
            Attempt::Completed => return Dispatched::HandledModern,
            Attempt::Reported(Detection::Undetermined) => return Dispatched::HandledUndetermined,
            Attempt::Reported(_) => return Dispatched::HandledModern,
        }
    }
}

/// Everything one attempt needs beyond the shared client and parameters.
struct AttemptCtx<'a> {
    /// Where a `tools/list` response is remembered for later promotion.
    schemas: &'a ToolSchemas,
    envelope: &'a Envelope,
    /// The id this attempt puts on the wire — fresh on every re-issue.
    wire_id: &'a Option<RequestId>,
    /// The id the caller is waiting on, which responses are retargeted onto.
    caller_id: &'a Option<RequestId>,
    /// Only true while the peer's era is still unknown.
    allow_fallback: bool,
    max_buffer_size: usize,
}

/// One POST and its response handling.
async fn send_once(
    client: &reqwest::Client,
    params: &ModernHttpParameters,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    ctx: &AttemptCtx<'_>,
) -> Attempt {
    let AttemptCtx {
        schemas: _,
        envelope,
        wire_id,
        caller_id,
        allow_fallback,
        max_buffer_size,
    } = *ctx;
    let body = match wire_id {
        Some(id) => JsonRpcMessage::Request(create_request(
            &envelope.method,
            Some(envelope.params.clone()),
            Some(id.clone()),
            None,
        )),
        None => JsonRpcMessage::Notification(create_notification(
            &envelope.method,
            Some(envelope.params.clone()),
        )),
    };

    let mut request = client
        .post(&params.url)
        .header("Content-Type", "application/json")
        // Both content types are mandatory: the server chooses per request
        // whether to answer with a single object or a stream.
        .header("Accept", "application/json, text/event-stream");

    for (key, value) in params.effective_headers() {
        if !matches!(key.as_str(), "Content-Type" | "Accept") {
            request = request.header(key, value);
        }
    }
    // Mirrored headers last, so no user header can shadow them.
    for (key, value) in &envelope.headers {
        request = request.header(key, value);
    }

    let response = match request.json(&body.to_value()).send().await {
        Ok(response) => response,
        Err(e) => {
            // A connection-level failure says nothing about the protocol.
            route_error(
                incoming_tx,
                caller_id,
                LOCAL_TRANSPORT_FAILURE,
                &e.to_string(),
            )
            .await;
            return Attempt::Reported(Detection::Undetermined);
        }
    };

    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();

    if status >= 400 {
        // An authorization challenge is answered before the response is
        // treated as a failure: the request has not been refused so much as
        // deferred until a credential is attached.
        #[cfg(feature = "auth")]
        if matches!(status, 401 | 403) {
            if let Some(session) = &params.auth {
                let challenge = response
                    .headers()
                    .get("WWW-Authenticate")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string);
                match session.handle_response(status, challenge.as_deref()).await {
                    Ok(crate::auth::Outcome::Retry) => return Attempt::Reauthorized,
                    Ok(crate::auth::Outcome::GiveUp) => {}
                    Err(e) => {
                        route_error(
                            incoming_tx,
                            caller_id,
                            LOCAL_TRANSPORT_FAILURE,
                            &format!("authorization failed: {e}"),
                        )
                        .await;
                        return Attempt::Reported(Detection::Undetermined);
                    }
                }
            }
        }

        let text = read_body_bounded(response, max_buffer_size, "HTTP error response")
            .await
            .unwrap_or_default();

        // While the era is still unknown, an error response is also the probe.
        // Only a *recognised modern* error proves a modern peer; anything else
        // means fall back rather than surface a failure the legacy lifecycle
        // would have handled fine.
        if allow_fallback && era::classify_http_response(status, &text) == Detection::Legacy {
            tracing::debug!("HTTP {status} identifies a legacy peer; falling back");
            return Attempt::NotModern;
        }

        // Route the server's own JSON-RPC error when it sent one, so callers
        // see -32020/-32021/-32022 rather than an opaque transport failure.
        if let Ok(message) = parse_message_str(&text) {
            if matches!(message, JsonRpcMessage::Error(_)) {
                let _ = incoming_tx.send(retarget(message, caller_id)).await;
                return Attempt::Reported(era::classify_http_response(status, &text));
            }
        }
        route_error(
            incoming_tx,
            caller_id,
            LOCAL_TRANSPORT_FAILURE,
            &format!("HTTP {status}: {text}"),
        )
        .await;
        return Attempt::Reported(era::classify_http_response(status, &text));
    }

    if content_type.contains("text/event-stream") {
        return stream_response(response, incoming_tx, wire_id, caller_id, max_buffer_size).await;
    }

    let text = match read_body_bounded(response, max_buffer_size, "HTTP response").await {
        Ok(text) => text,
        Err(e) => {
            route_error(
                incoming_tx,
                caller_id,
                LOCAL_MALFORMED_RESPONSE,
                &e.to_string(),
            )
            .await;
            return Attempt::Reported(Detection::Modern);
        }
    };
    if text.trim().is_empty() {
        // 202 Accepted for a notification. A request with an empty body would
        // otherwise hang the caller, so synthesise an empty success.
        if let Some(id) = caller_id {
            let _ = incoming_tx
                .send(JsonRpcMessage::Response(create_response(id.clone(), None)))
                .await;
        }
        return Attempt::Completed;
    }

    match parse_message_str(&text) {
        Ok(message) => {
            // A listing seen on the way past is what makes the *next*
            // `tools/call` able to promote its parameters.
            ctx.schemas.observe(&ctx.envelope.method, &message);
            let _ = incoming_tx.send(retarget(message, caller_id)).await;
            Attempt::Completed
        }
        Err(e) => {
            // A 2xx arrived, so the peer is modern; its body was just unusable.
            route_error(
                incoming_tx,
                caller_id,
                LOCAL_MALFORMED_RESPONSE,
                &format!("Parse error: {e}"),
            )
            .await;
            Attempt::Reported(Detection::Modern)
        }
    }
}

/// Consume an SSE response stream, reporting whether a final response arrived.
///
/// Notifications on this stream belong to the originating request (progress,
/// log messages) and are forwarded untouched. Only the final response is
/// retargeted onto the caller's id.
async fn stream_response(
    response: reqwest::Response,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    wire_id: &Option<RequestId>,
    caller_id: &Option<RequestId>,
    max_buffer_size: usize,
) -> Attempt {
    use futures::StreamExt;

    let mut buffer = String::new();
    let mut stream = response.bytes_stream();
    let mut saw_response = false;

    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else { break };
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        // A peer that never sends an event boundary would otherwise grow this
        // buffer until the process dies. Bound it and give up on the stream.
        if max_buffer_size > 0 && buffer.len() > max_buffer_size {
            route_error(
                incoming_tx,
                caller_id,
                LOCAL_MALFORMED_RESPONSE,
                &format!(
                    "SSE event exceeded the {max_buffer_size}-byte buffer limit \
                     without a boundary"
                ),
            )
            .await;
            return Attempt::Reported(Detection::Modern);
        }

        while let Some(pos) = find_event_boundary(&buffer) {
            let event: String = buffer.drain(..pos).collect();
            if handle_event(&event, incoming_tx, wire_id, caller_id).await {
                saw_response = true;
            }
        }
    }
    if !buffer.trim().is_empty() && handle_event(&buffer, incoming_tx, wire_id, caller_id).await {
        saw_response = true;
    }

    match (saw_response, caller_id) {
        (true, _) => Attempt::Completed,
        // A notification expects nothing back, so a closed stream is normal.
        (false, None) => Attempt::Completed,
        (false, Some(_)) => Attempt::StreamBroken,
    }
}

/// Handle one SSE event. Returns whether it carried the final response.
async fn handle_event(
    event_text: &str,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    wire_id: &Option<RequestId>,
    caller_id: &Option<RequestId>,
) -> bool {
    let mut event_type: Option<&str> = None;
    let mut data_lines: Vec<&str> = Vec::new();

    for line in event_text.lines() {
        let line = line.trim_end_matches('\r');
        // A line starting with ':' is an SSE comment — servers send them as
        // keep-alives on long-lived streams and they must be ignored.
        if line.starts_with(':') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.trim());
        } else if let Some(rest) = line.strip_prefix("data:") {
            data_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
        }
    }

    if data_lines.is_empty() || !matches!(event_type, Some("message" | "response") | None) {
        return false;
    }
    let data = data_lines.join("\n");
    if !data.trim_start().starts_with('{') {
        return false;
    }

    let Ok(message) = parse_message_str(data.trim()) else {
        tracing::error!("failed to parse SSE message JSON");
        return false;
    };

    let is_final = matches!(
        message,
        JsonRpcMessage::Response(_) | JsonRpcMessage::Error(_)
    ) && message.id() == wire_id.as_ref();

    let _ = incoming_tx
        .send(if is_final {
            retarget(message, caller_id)
        } else {
            message
        })
        .await;
    is_final
}

/// Rewrite a response's id onto the caller's, undoing a retry's fresh wire id.
///
/// Without this a re-issued request would answer under an id the caller never
/// used, and the original call would wait forever.
fn retarget(message: JsonRpcMessage, caller_id: &Option<RequestId>) -> JsonRpcMessage {
    let Some(id) = caller_id else {
        return message;
    };
    match message {
        JsonRpcMessage::Response(mut response) => {
            response.id = id.clone();
            JsonRpcMessage::Response(response)
        }
        JsonRpcMessage::Error(mut error) => {
            error.id = id.clone();
            JsonRpcMessage::Error(error)
        }
        other => other,
    }
}

/// Find the end of the first complete SSE event, past its blank-line separator.
fn find_event_boundary(buffer: &str) -> Option<usize> {
    let lf = buffer.find("\n\n").map(|i| i + 2);
    let crlf = buffer.find("\r\n\r\n").map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Synthesise an error response for the caller's id.
async fn route_error(
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    caller_id: &Option<RequestId>,
    code: i64,
    message: &str,
) {
    tracing::debug!("modern HTTP transport error for {caller_id:?}: {message}");
    if let Some(id) = caller_id {
        let _ = incoming_tx
            .send(JsonRpcMessage::Error(create_error_response(
                id.clone(),
                code,
                message,
                None,
            )))
            .await;
    }
}

#[async_trait]
impl Transport for ModernHttpTransport {
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

impl Drop for ModernHttpTransport {
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
        assert!(ModernHttpParameters::new("").is_err());
        assert!(ModernHttpParameters::new("ftp://x").is_err());
        assert!(ModernHttpParameters::new("http://x").is_ok());
        // Trailing slash is normalised away.
        assert_eq!(
            ModernHttpParameters::new("https://x/mcp/").unwrap().url,
            "https://x/mcp"
        );
    }

    #[test]
    fn defaults_are_modern() {
        let p = ModernHttpParameters::new("http://x").unwrap();
        assert_eq!(p.protocol_version, versioning::FIRST_MODERN_VERSION);
        assert!(p.identity.info.is_some());
        assert_eq!(p.max_stream_retries, 2);
    }

    #[test]
    fn a_caller_supplied_session_header_is_dropped() {
        // There are no protocol sessions. Forwarding one could also make a
        // dual-era server pick legacy semantics for a modern request.
        let mut headers = HashMap::new();
        headers.insert("Mcp-Session-Id".to_string(), "abc".to_string());
        headers.insert("mcp-session-id".to_string(), "lower".to_string());
        headers.insert("X-Trace".to_string(), "keep-me".to_string());

        let effective = ModernHttpParameters::new("http://x")
            .unwrap()
            .with_headers(headers)
            .effective_headers();

        assert!(!effective
            .keys()
            .any(|k| k.eq_ignore_ascii_case("Mcp-Session-Id")));
        assert_eq!(
            effective.get("X-Trace").map(String::as_str),
            Some("keep-me")
        );
        assert!(effective.contains_key("User-Agent"));
    }

    #[test]
    fn identity_can_be_overridden() {
        use crate::protocol::types::info::ClientInfo;

        let identity = ClientIdentity {
            info: Some(ClientInfo {
                name: "my-client".into(),
                version: "9.9".into(),
                ..ClientInfo::default()
            }),
            ..ClientIdentity::chuk()
        };
        let p = ModernHttpParameters::new("http://x")
            .unwrap()
            .with_identity(identity);
        assert_eq!(p.identity.info.as_ref().unwrap().name, "my-client");
    }

    #[test]
    fn bearer_token_becomes_an_authorization_header() {
        let effective = ModernHttpParameters::new("http://x")
            .unwrap()
            .with_bearer_token("tok")
            .effective_headers();
        assert_eq!(
            effective.get("Authorization").map(String::as_str),
            Some("Bearer tok")
        );
    }

    #[test]
    fn retarget_maps_a_retry_response_back_to_the_caller() {
        let caller = Some(RequestId::Num(1));
        let wire = RequestId::Str("retry-uuid".into());

        let response = JsonRpcMessage::Response(create_response(wire.clone(), None));
        assert_eq!(
            retarget(response, &caller).id(),
            Some(&RequestId::Num(1)),
            "response id should be rewritten onto the caller's"
        );

        let error = JsonRpcMessage::Error(create_error_response(
            wire,
            LOCAL_TRANSPORT_FAILURE,
            "x",
            None,
        ));
        assert_eq!(retarget(error, &caller).id(), Some(&RequestId::Num(1)));

        // Notifications have no id and pass through untouched.
        let note =
            JsonRpcMessage::Notification(create_notification("notifications/progress", None));
        assert!(retarget(note, &caller).id().is_none());
    }

    #[test]
    fn event_boundaries_handle_both_line_endings() {
        assert_eq!(find_event_boundary("a\n\nb"), Some(3));
        assert_eq!(find_event_boundary("a\r\n\r\nb"), Some(5));
        assert_eq!(find_event_boundary("no boundary"), None);
    }
}
