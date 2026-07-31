//! A transport that records everything the client sends.
//!
//! Client-side conformance is a question about the wire, so the only way to
//! answer it honestly is to look at the bytes the client actually emits —
//! not at the arguments it was called with.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use chuk_mcp::protocol::json_rpc::{create_response, JsonRpcMessage};
use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use chuk_mcp::protocol::versioning;
use chuk_mcp::transports::Transport;
use chuk_mcp::McpError;

/// Channel depth for the recorder. Larger than any rule's traffic, so a rule
/// can never be measuring back-pressure by accident.
const CHANNEL_DEPTH: usize = 64;

/// Server identity the canned responses report.
const SERVER_NAME: &str = "conformance-peer";
const SERVER_VERSION: &str = "0.0.0";

/// The tool the canned `tools/list` advertises.
pub const TOOL_NAME: &str = "greet";

/// A transport that answers like a minimal legacy server while keeping every
/// message the client sent.
pub struct RecordingTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
    sent: Arc<Mutex<Vec<JsonRpcMessage>>>,
    /// The protocol version the client handed down after negotiating.
    negotiated: Arc<Mutex<Option<String>>>,
}

impl RecordingTransport {
    /// Start a recorder that negotiates `protocol_version`.
    pub fn new(protocol_version: &'static str) -> Self {
        let (inject, incoming) = message_channel(CHANNEL_DEPTH);
        let (outgoing, mut outbound) = mpsc::channel::<JsonRpcMessage>(CHANNEL_DEPTH);
        let sent: Arc<Mutex<Vec<JsonRpcMessage>>> = Arc::new(Mutex::new(Vec::new()));

        let recorded = sent.clone();
        tokio::spawn(async move {
            while let Some(message) = outbound.recv().await {
                recorded
                    .lock()
                    .expect("recorder mutex poisoned")
                    .push(message.clone());

                // Notifications get no reply, which is itself a rule.
                let Some(id) = message.id().cloned() else {
                    continue;
                };
                let result = canned_result(message.method().unwrap_or_default(), protocol_version);
                let _ = inject
                    .send(JsonRpcMessage::Response(create_response(id, Some(result))))
                    .await;
            }
        });

        RecordingTransport {
            incoming,
            outgoing,
            sent,
            negotiated: Arc::new(Mutex::new(None)),
        }
    }

    /// A handle to the recording, read after the client is consumed — the
    /// client takes ownership of the transport, so the rules keep this
    /// instead of the transport itself.
    pub fn recording(&self) -> Arc<Mutex<Vec<JsonRpcMessage>>> {
        self.sent.clone()
    }

    /// The version the client pushed down after negotiation, if any.
    pub fn negotiated_version(&self) -> Arc<Mutex<Option<String>>> {
        self.negotiated.clone()
    }
}

#[async_trait]
impl Transport for RecordingTransport {
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), McpError> {
        Ok((self.incoming.clone(), self.outgoing.clone()))
    }

    fn set_protocol_version(&self, version: &str) {
        *self.negotiated.lock().expect("recorder mutex poisoned") = Some(version.to_string());
    }
}

/// The minimum a legacy peer must return for each method the rules exercise.
fn canned_result(method: &str, protocol_version: &str) -> Value {
    match method {
        MessageMethod::INITIALIZE => json!({
            "protocolVersion": protocol_version,
            "serverInfo": {"name": SERVER_NAME, "version": SERVER_VERSION},
            "capabilities": {"tools": {}, "resources": {}},
        }),
        MessageMethod::TOOLS_LIST => json!({
            "tools": [{"name": TOOL_NAME, "inputSchema": {"type": "object"}}],
        }),
        MessageMethod::TOOLS_CALL => json!({
            "content": [{"type": "text", "text": "ok"}],
        }),
        _ => json!({}),
    }
}

/// The version a recorder negotiates unless a rule needs another.
pub const DEFAULT_NEGOTIATED_VERSION: &str = versioning::V2025_06_18;
