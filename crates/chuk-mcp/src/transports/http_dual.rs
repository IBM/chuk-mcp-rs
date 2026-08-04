//! Dual-era Streamable HTTP transport: speaks `2026-07-28` where it can and the
//! legacy stateful shape where it must.
//!
//! ## Why the era is not decided up front
//!
//! On stdio a client can probe with `server/discover` before anything else. HTTP
//! has no equivalent: there is no pre-flight request that distinguishes a modern
//! server from a legacy one without also *being* a real request. The spec is
//! explicit that a client should attempt a modern request and inspect the body of
//! a `400` before falling back — so **the first real call is the probe**, and a
//! separate probe would cost a round trip on every connection for information
//! the first call already yields.
//!
//! This transport therefore starts modern with the era unknown, and classifies
//! the first response:
//!
//! * A success, or a recognised modern error (`-32020`/`-32021`/`-32022`),
//!   proves a modern peer. Cached, and the error reaches the caller.
//! * A `400` without a modern error, a bare `404`, a `405` — the peer is legacy.
//!   Cached, and the request is **re-sent** through the legacy transport.
//! * A `401`, `429`, `5xx` and so on prove nothing about era. Nothing is cached
//!   and the error is surfaced, so a sick or unauthenticated server is never
//!   mistaken for an old one.
//!
//! Re-sending on fallback is safe rather than a double-execution risk: every
//! response that yields the legacy verdict means the request was rejected
//! *before* being processed.
//!
//! Once an era is cached, subsequent requests go straight down that path and a
//! `400` is treated as the real error it is. The cache is keyed on
//! `(endpoint, credential context)` and invalidated by `-32022`, so a server
//! upgraded underneath a running client is re-detected rather than stuck.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, Semaphore};

use crate::protocol::envelope::ClientIdentity;
use crate::protocol::era::{EndpointKey, EraCache, EraMode, ProtocolEra};
use crate::protocol::json_rpc::JsonRpcMessage;
use crate::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use crate::protocol::tool_schemas::ToolSchemas;
use crate::protocol::types::errors::McpError;
use crate::protocol::versioning;
use crate::transports::http::{self, StreamableHttpParameters};
use crate::transports::http_listen;
use crate::transports::http_modern::{self, Dispatched, ModernHttpParameters};
use crate::transports::limits::TransportLimits;
use crate::transports::Transport;

/// Parameters for the dual-era Streamable HTTP transport.
#[derive(Debug, Clone)]
pub struct DualEraHttpParameters {
    pub url: String,
    pub headers: HashMap<String, String>,
    pub timeout: f64,
    pub bearer_token: Option<String>,
    /// Authorization state for this endpoint, when OAuth is in play.
    #[cfg(feature = "auth")]
    pub auth: Option<Arc<crate::auth::AuthSession>>,
    pub max_concurrent_requests: usize,
    pub identity: ClientIdentity,
    /// `Auto` detects; `Legacy` and `Modern` pin and never probe.
    pub mode: EraMode,
    /// An opaque, stable identity for the credential in use — **never a raw
    /// token**. Part of the cache key because one endpoint can serve different
    /// eras to different principals.
    pub credential_context: Option<String>,
    pub max_stream_retries: usize,
}

impl DualEraHttpParameters {
    pub fn new(url: impl Into<String>) -> Result<Self, McpError> {
        // Validate through the modern parameters so the rules stay in one place.
        let modern = ModernHttpParameters::new(url)?;
        Ok(DualEraHttpParameters {
            url: modern.url,
            headers: HashMap::new(),
            timeout: modern.timeout,
            bearer_token: None,
            #[cfg(feature = "auth")]
            auth: None,
            max_concurrent_requests: modern.max_concurrent_requests,
            identity: ClientIdentity::chuk(),
            mode: EraMode::Auto,
            credential_context: None,
            max_stream_retries: modern.max_stream_retries,
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

    pub fn with_mode(mut self, mode: EraMode) -> Self {
        self.mode = mode;
        self
    }

    /// Scope the era decision to a credential context. See the field docs.
    pub fn with_credential_context(mut self, context: impl Into<String>) -> Self {
        self.credential_context = Some(context.into());
        self
    }

    fn endpoint_key(&self) -> EndpointKey {
        match &self.credential_context {
            Some(context) => EndpointKey::new(&self.url, context),
            None => EndpointKey::anonymous(&self.url),
        }
    }

    fn modern(&self) -> ModernHttpParameters {
        let mut params = ModernHttpParameters::new(&self.url).expect("url already validated");
        params.headers = self.headers.clone();
        params.timeout = self.timeout;
        params.bearer_token = self.bearer_token.clone();
        #[cfg(feature = "auth")]
        {
            params.auth = self.auth.clone();
        }
        params.max_concurrent_requests = self.max_concurrent_requests;
        params.identity = self.identity.clone();
        params.protocol_version = versioning::FIRST_MODERN_VERSION.to_string();
        params.max_stream_retries = self.max_stream_retries;
        params
    }

    fn legacy(&self) -> StreamableHttpParameters {
        let mut params = StreamableHttpParameters::new(&self.url).expect("url already validated");
        params.headers = self.headers.clone();
        params.timeout = self.timeout;
        params.bearer_token = self.bearer_token.clone();
        #[cfg(feature = "auth")]
        {
            params.auth = self.auth.clone();
        }
        params.max_concurrent_requests = self.max_concurrent_requests;
        params
    }
}

/// Everything opening the legacy `GET` stream needs.
///
/// Held so the stream can be opened from either direction: lazily, by the
/// first legacy request, or eagerly, the moment a caller tells the transport
/// the peer is legacy.
#[derive(Clone)]
pub(crate) struct LegacyStream {
    client: reqwest::Client,
    parameters: DualEraHttpParameters,
    session: Arc<std::sync::Mutex<Option<String>>>,
    incoming_tx: mpsc::Sender<JsonRpcMessage>,
    max_buffer_size: usize,
    listener: Arc<LegacyListener>,
}

impl LegacyStream {
    /// Open the stream if it is not already open, and hand back the hints a
    /// request should carry.
    /// Wait for the stream to settle, if one has been opened.
    async fn wait_ready(&self) {
        self.listener.ready.wait().await;
    }

    fn ensure(&self) -> Arc<http_listen::StreamHints> {
        self.listener.ensure(
            &self.client,
            &self.parameters,
            &self.session,
            &self.incoming_tx,
            self.max_buffer_size,
        )
    }
}

/// A Streamable HTTP transport that works against both protocol eras.
pub struct DualEraHttpTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
    era_cache: Arc<EraCache>,
    key: EndpointKey,
    legacy_stream: LegacyStream,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl DualEraHttpTransport {
    pub fn start(parameters: DualEraHttpParameters) -> Result<Self, McpError> {
        Self::start_with_limits(parameters, TransportLimits::default())
    }

    /// Start the transport with explicit buffer limits, applied to both eras.
    pub fn start_with_limits(
        parameters: DualEraHttpParameters,
        limits: TransportLimits,
    ) -> Result<Self, McpError> {
        let (incoming_tx, incoming) = message_channel(100);
        let (outgoing, mut outgoing_rx) = mpsc::channel::<JsonRpcMessage>(100);

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs_f64(parameters.timeout))
            .build()
            .map_err(|e| McpError::Transport(format!("Failed to build HTTP client: {e}")))?;

        let era_cache = Arc::new(EraCache::new());
        let key = parameters.endpoint_key();

        // A legacy session id, if the peer turns out to be legacy and mints one.
        // Never touched on the modern path.
        let legacy_session = Arc::new(std::sync::Mutex::new(None::<String>));

        let semaphore = Arc::new(Semaphore::new(parameters.max_concurrent_requests.max(1)));
        let max_buffer_size = limits.max_buffer_size;
        let cache_for_task = era_cache.clone();
        let key_for_task = key.clone();
        // Shared across every request on this connection: a `tools/list` seen
        // once is what lets a later `tools/call` promote its parameters.
        let schemas = ToolSchemas::new();
        let legacy_stream = LegacyStream {
            client: client.clone(),
            parameters: parameters.clone(),
            session: legacy_session.clone(),
            incoming_tx: incoming_tx.clone(),
            max_buffer_size,
            listener: Arc::new(LegacyListener::default()),
        };
        let stream_for_task = legacy_stream.clone();

        let task = tokio::spawn(async move {
            while let Some(message) = outgoing_rx.recv().await {
                let permit = semaphore.clone().acquire_owned().await;
                let client = client.clone();
                let params = parameters.clone();
                let incoming_tx = incoming_tx.clone();
                let cache = cache_for_task.clone();
                let key = key_for_task.clone();
                let session = legacy_session.clone();
                let schemas = schemas.clone();
                let legacy_stream = stream_for_task.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    route(
                        &client,
                        &params,
                        &cache,
                        &key,
                        &session,
                        &incoming_tx,
                        message,
                        max_buffer_size,
                        &schemas,
                        &legacy_stream,
                    )
                    .await;
                });
            }
        });

        Ok(DualEraHttpTransport {
            incoming,
            outgoing,
            era_cache,
            key,
            legacy_stream,
            task: Some(task),
        })
    }

    /// The era decided for this endpoint, if one has been decided yet.
    ///
    /// `None` under [`EraMode::Auto`] until the first response classifies the
    /// peer — HTTP cannot know sooner.
    pub fn era(&self) -> Option<ProtocolEra> {
        self.era_cache.get(&self.key)
    }

    /// Record which era this endpoint speaks.
    ///
    /// The transport works the era out per request, but a caller may learn it
    /// first: [`crate::connect`] settles the handshake before handing the
    /// client over, and what it discovered there has to reach the transport or
    /// the two disagree.
    ///
    /// The disagreement is not academic. A legacy server that answers
    /// `server/discover` with an ordinary `-32601` — an HTTP `200` carrying a
    /// JSON-RPC error — completes the request as far as the modern path is
    /// concerned, so the transport concludes "modern" while the handshake
    /// concluded "legacy". The visible symptom is a client that never opens
    /// the `GET` stream and so never hears a pushed `elicitation/create`.
    pub fn set_era(&self, era: ProtocolEra) {
        self.era_cache.insert(self.key.clone(), era);
        if era == ProtocolEra::Legacy {
            // Opened now rather than on the next request: a server may push
            // before the client asks for anything else, and a stream opened
            // after that push has already missed it.
            self.legacy_stream.ensure();
        }
    }
}

/// Send one message down whichever era applies, falling back if needed.
#[allow(clippy::too_many_arguments)]
async fn route(
    client: &reqwest::Client,
    params: &DualEraHttpParameters,
    cache: &EraCache,
    key: &EndpointKey,
    legacy_session: &Arc<std::sync::Mutex<Option<String>>>,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    message: JsonRpcMessage,
    max_buffer_size: usize,
    schemas: &ToolSchemas,
    legacy_stream: &LegacyStream,
) {
    // A pinned mode ignores both the cache and detection — that is the point of
    // pinning, and why it works behind a gateway that mangles the 400 body.
    let era = params.mode.resolve(cache.get(key));

    if era == Some(ProtocolEra::Legacy) {
        send_legacy(
            client,
            params,
            legacy_session,
            incoming_tx,
            message,
            max_buffer_size,
            legacy_stream,
        )
        .await;
        return;
    }

    // Modern, or era still unknown. Fallback is allowed only in the unknown
    // case: once known-modern, a 400 is a real error for the caller.
    let unknown = era.is_none();
    let outcome = http_modern::dispatch_modern(
        client,
        &params.modern(),
        incoming_tx,
        message.clone(),
        unknown,
        max_buffer_size,
        schemas,
    )
    .await;

    match outcome {
        Dispatched::HandledModern => {
            if unknown {
                // The peer answered as only a modern server can.
                cache.insert(key.clone(), ProtocolEra::Modern);
            }
        }
        Dispatched::HandledUndetermined => {
            // A timeout, a 401, a 502. The caller has its error; we learned
            // nothing, so the next request probes again rather than inheriting
            // a guess made while the server was unhealthy.
        }
        Dispatched::FallBackToLegacy => {
            tracing::debug!("{} is a legacy peer; retrying under initialize", params.url);
            cache.insert(key.clone(), ProtocolEra::Legacy);
            // Safe to re-send: the modern attempt was rejected before the
            // server processed it.
            send_legacy(
                client,
                params,
                legacy_session,
                incoming_tx,
                message,
                max_buffer_size,
                legacy_stream,
            )
            .await;
        }
    }
}

/// The legacy server-to-client stream, and whether it has been opened.
///
/// The 2026 revision removed the `GET` endpoint, so this transport cannot open
/// one up front the way [`crate::transports::http`] does: until the era is
/// known, a `GET` would be a request the peer may have no answer for. It is
/// started instead at the first moment the peer is known to be legacy — which
/// is before any legacy request is sent, and therefore before the server has
/// anything to push back.
#[derive(Default)]
pub(crate) struct LegacyListener {
    started: std::sync::atomic::AtomicBool,
    hints: Arc<http_listen::StreamHints>,
    pub(crate) ready: Arc<http_listen::Ready>,
}

impl LegacyListener {
    /// Open the stream if it is not already open, and hand back the hints a
    /// request should carry.
    fn ensure(
        &self,
        client: &reqwest::Client,
        params: &DualEraHttpParameters,
        legacy_session: &Arc<std::sync::Mutex<Option<String>>>,
        incoming_tx: &mpsc::Sender<JsonRpcMessage>,
        max_buffer_size: usize,
    ) -> Arc<http_listen::StreamHints> {
        // `swap` rather than load-then-store: two requests racing to be the
        // first legacy one must not open two streams.
        if !self.started.swap(true, std::sync::atomic::Ordering::SeqCst) {
            tracing::debug!("{} is legacy; opening the GET stream", params.url);
            tokio::spawn(http_listen::listen(
                client.clone(),
                params.legacy(),
                legacy_session.clone(),
                incoming_tx.clone(),
                max_buffer_size,
                self.ready.clone(),
                self.hints.clone(),
            ));
        }
        self.hints.clone()
    }
}

async fn send_legacy(
    client: &reqwest::Client,
    params: &DualEraHttpParameters,
    legacy_session: &Arc<std::sync::Mutex<Option<String>>>,
    incoming_tx: &mpsc::Sender<JsonRpcMessage>,
    message: JsonRpcMessage,
    max_buffer_size: usize,
    legacy_stream: &LegacyStream,
) {
    // A legacy server may push `sampling/createMessage` or
    // `elicitation/create` at any time, and it does so on the `GET` stream. A
    // client that never opened one simply never hears them — so opening it is
    // part of speaking the era, not an optimisation.
    let hints = legacy_stream.ensure();

    http::send_via_http(
        client,
        &params.legacy(),
        legacy_session,
        incoming_tx,
        message,
        max_buffer_size,
        Some(&hints),
    )
    .await;
}

#[async_trait]
impl Transport for DualEraHttpTransport {
    /// Wait until a legacy peer's `GET` stream is live.
    ///
    /// Only meaningful once the era is known: a modern peer has no such stream
    /// and would wait for something that is never coming. Against a legacy one
    /// this is what stops the first request racing the stream that carries the
    /// server's reply to it — the stream is opened as soon as the era settles,
    /// but opening it takes a round trip the caller would otherwise outrun.
    async fn ready(&self) {
        if self.era() == Some(ProtocolEra::Legacy) {
            self.legacy_stream.wait_ready().await;
        }
    }

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

impl Drop for DualEraHttpTransport {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> DualEraHttpParameters {
        DualEraHttpParameters::new("http://example.test/mcp").unwrap()
    }

    #[test]
    fn defaults_to_auto_detection() {
        let p = params();
        assert_eq!(p.mode, EraMode::Auto);
        assert!(p.mode.requires_detection());
        assert!(p.credential_context.is_none());
    }

    #[test]
    fn url_validation_is_shared_with_the_modern_transport() {
        assert!(DualEraHttpParameters::new("").is_err());
        assert!(DualEraHttpParameters::new("ftp://x").is_err());
        assert_eq!(
            DualEraHttpParameters::new("https://x/mcp/").unwrap().url,
            "https://x/mcp"
        );
    }

    #[test]
    fn derived_parameters_carry_the_shared_settings() {
        let mut headers = HashMap::new();
        headers.insert("X-Tenant".to_string(), "acme".to_string());
        let p = params()
            .with_headers(headers)
            .with_bearer_token("tok")
            .with_credential_context("issuer|alice");

        let modern = p.modern();
        assert_eq!(modern.bearer_token.as_deref(), Some("tok"));
        assert_eq!(modern.protocol_version, versioning::FIRST_MODERN_VERSION);
        assert_eq!(
            modern.headers.get("X-Tenant").map(String::as_str),
            Some("acme")
        );

        let legacy = p.legacy();
        assert_eq!(legacy.bearer_token.as_deref(), Some("tok"));
        assert_eq!(
            legacy.headers.get("X-Tenant").map(String::as_str),
            Some("acme")
        );
    }

    #[test]
    fn the_cache_key_includes_the_credential_context() {
        // Two principals on one endpoint must not share an era decision.
        let anonymous = params().endpoint_key();
        let alice = params()
            .with_credential_context("issuer|alice")
            .endpoint_key();
        let bob = params()
            .with_credential_context("issuer|bob")
            .endpoint_key();

        assert_ne!(anonymous, alice);
        assert_ne!(alice, bob);
        assert_eq!(alice.endpoint, bob.endpoint);
    }

    #[test]
    fn pinning_bypasses_detection_entirely() {
        // A pinned mode must resolve without consulting the cache, which is
        // what makes it usable behind a gateway that rewrites the 400 body.
        assert_eq!(
            params().with_mode(EraMode::Legacy).mode.resolve(None),
            Some(ProtocolEra::Legacy)
        );
        assert_eq!(
            params().with_mode(EraMode::Modern).mode.resolve(None),
            Some(ProtocolEra::Modern)
        );
        // Auto has nothing to go on until a response arrives.
        assert_eq!(params().mode.resolve(None), None);
    }
}
