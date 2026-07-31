//! The Phase 4 gate: identical caller code drives both eras.
//!
//! Two fake servers ask for the same thing in the two different ways the
//! protocol allows — a modern one returns an `input_required` result and waits
//! to be retried, a legacy one pushes an `elicitation/create` request mid-call
//! — and the *same* client code with the *same* handler completes both.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;

use chuk_mcp::client::input::InputHandler;
use chuk_mcp::client::McpClient;
use chuk_mcp::protocol::era::{ProtocolEra, ServerProfile};
use chuk_mcp::protocol::json_rpc::{create_request, create_response, JsonRpcMessage, RequestId};
use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use chuk_mcp::protocol::mrtr::{ElicitRequest, ElicitResult};
use chuk_mcp::transports::Transport;
use chuk_mcp::McpError;

const CHANNEL_DEPTH: usize = 32;

/// The tool both servers expose, and what it needs to know.
const TOOL_NAME: &str = "open_pull_request";
const ELICIT_KEY: &str = "github_login";
const ELICIT_FIELD: &str = "name";
const ELICIT_ANSWER: &str = "octocat";

/// The opaque state the modern server hands out. Its exact bytes are the point:
/// the client must echo them back unaltered.
const REQUEST_STATE: &str = "eyJsb2NhdGlvbiI6Ik5ldyBZb3JrIn0...==";

/// What both servers finally return, so the two eras are compared on identical
/// output.
fn final_text() -> String {
    format!("Opened a pull request as {ELICIT_ANSWER}")
}

fn elicit_request_params() -> Value {
    json!({
        "mode": "form",
        "message": "Please provide your GitHub username",
        "requestedSchema": {
            "type": "object",
            "properties": {ELICIT_FIELD: {"type": "string"}},
            "required": [ELICIT_FIELD],
        },
    })
}

// --- the caller's handler, shared by both eras ---------------------------

/// Answers the elicitation with a fixed username, and records what it was
/// asked so the test can prove the request survived the trip in either shape.
struct Reviewer {
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl InputHandler for Reviewer {
    async fn elicit(&self, request: ElicitRequest) -> ElicitResult {
        self.seen
            .lock()
            .expect("handler mutex")
            .push(request.message.clone());

        let mut content = Map::new();
        content.insert(ELICIT_FIELD.to_string(), json!(ELICIT_ANSWER));
        ElicitResult::accept(content)
    }
}

// --- a transport backed by a scripted server -----------------------------

/// What the server saw, for assertions after the call.
#[derive(Default)]
struct Journal {
    /// Ids of every `tools/call` the client sent.
    call_ids: Vec<String>,
    /// The `requestState` on each retry, exactly as received.
    echoed_state: Vec<Option<Value>>,
    /// The `inputResponses` on each retry.
    responses: Vec<Option<Value>>,
}

type Log = Arc<Mutex<Journal>>;

struct ScriptedTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
}

#[async_trait]
impl Transport for ScriptedTransport {
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), McpError> {
        Ok((self.incoming.clone(), self.outgoing.clone()))
    }
}

/// A `2026-07-28` server: answers the first `tools/call` with `input_required`
/// and completes the second.
fn modern_server() -> (ScriptedTransport, Log) {
    let (inject, incoming) = message_channel(CHANNEL_DEPTH);
    let (outgoing, mut outbound) = mpsc::channel::<JsonRpcMessage>(CHANNEL_DEPTH);
    let log: Log = Arc::new(Mutex::new(Journal::default()));

    let journal = log.clone();
    tokio::spawn(async move {
        while let Some(message) = outbound.recv().await {
            let Some(id) = message.id().cloned() else {
                continue;
            };
            if message.method() != Some(MessageMethod::TOOLS_CALL) {
                let _ = inject
                    .send(JsonRpcMessage::Response(create_response(
                        id,
                        Some(json!({})),
                    )))
                    .await;
                continue;
            }

            let params = message.params().cloned().unwrap_or(Value::Null);
            let answers = params.get("inputResponses").cloned();
            {
                let mut journal = journal.lock().expect("journal mutex");
                journal.call_ids.push(id.to_string());
                journal
                    .echoed_state
                    .push(params.get("requestState").cloned());
                journal.responses.push(answers.clone());
            }

            let result = match answers {
                // First attempt: ask, and hand out state to carry back.
                None => json!({
                    "resultType": "input_required",
                    "inputRequests": {
                        ELICIT_KEY: {
                            "method": MessageMethod::ELICITATION_CREATE,
                            "params": elicit_request_params(),
                        },
                    },
                    "requestState": REQUEST_STATE,
                }),
                // Retry: complete it.
                Some(_) => json!({
                    "resultType": "complete",
                    "content": [{"type": "text", "text": final_text()}],
                }),
            };

            let _ = inject
                .send(JsonRpcMessage::Response(create_response(id, Some(result))))
                .await;
        }
    });

    (ScriptedTransport { incoming, outgoing }, log)
}

/// A legacy server: pushes `elicitation/create` at the client mid-call, then
/// completes the original request once answered.
fn legacy_server() -> (ScriptedTransport, Log) {
    let (inject, incoming) = message_channel(CHANNEL_DEPTH);
    let (outgoing, mut outbound) = mpsc::channel::<JsonRpcMessage>(CHANNEL_DEPTH);
    let log: Log = Arc::new(Mutex::new(Journal::default()));

    let journal = log.clone();
    tokio::spawn(async move {
        // The id of the call being held open while the client is asked.
        let mut pending_call: Option<RequestId> = None;
        let push_id = RequestId::Str("server-push-1".to_string());

        while let Some(message) = outbound.recv().await {
            // The client's answer to our pushed request.
            if message.id() == Some(&push_id) && message.method().is_none() {
                let answer = message.result().cloned();
                {
                    let mut journal = journal.lock().expect("journal mutex");
                    journal.responses.push(answer);
                }
                if let Some(call_id) = pending_call.take() {
                    let _ = inject
                        .send(JsonRpcMessage::Response(create_response(
                            call_id,
                            Some(json!({
                                "content": [{"type": "text", "text": final_text()}],
                            })),
                        )))
                        .await;
                }
                continue;
            }

            let Some(id) = message.id().cloned() else {
                continue;
            };

            match message.method() {
                Some(MessageMethod::INITIALIZE) => {
                    let _ = inject
                        .send(JsonRpcMessage::Response(create_response(
                            id,
                            Some(json!({
                                "protocolVersion": "2025-06-18",
                                "serverInfo": {"name": "legacy-elicitor", "version": "1.0.0"},
                                "capabilities": {"tools": {}},
                            })),
                        )))
                        .await;
                }
                Some(MessageMethod::TOOLS_CALL) => {
                    journal
                        .lock()
                        .expect("journal mutex")
                        .call_ids
                        .push(id.to_string());
                    pending_call = Some(id);

                    // Hold the call open and ask the client, the only way a
                    // legacy server can.
                    let _ = inject
                        .send(JsonRpcMessage::Request(create_request(
                            MessageMethod::ELICITATION_CREATE,
                            Some(elicit_request_params()),
                            Some(push_id.clone()),
                            None,
                        )))
                        .await;
                }
                _ => {
                    let _ = inject
                        .send(JsonRpcMessage::Response(create_response(
                            id,
                            Some(json!({})),
                        )))
                        .await;
                }
            }
        }
    });

    (ScriptedTransport { incoming, outgoing }, log)
}

/// The caller's code. Byte-identical between the two tests below, which is the
/// whole point of the phase.
async fn open_pull_request(mut client: McpClient, handler: Arc<dyn InputHandler>) -> String {
    client.set_input_handler(handler);
    let result = client
        .call_tool(TOOL_NAME, json!({"repo": "chuk-mcp-rs"}))
        .await
        .expect("the call completes");
    result.text()
}

/// Build a settled client over a scripted transport, the way `connect` does.
async fn settled(transport: ScriptedTransport, era: ProtocolEra) -> McpClient {
    let (read, write) = transport.get_streams().await.expect("streams");
    McpClient::from_profile(
        transport,
        read,
        write,
        ServerProfile {
            era,
            protocol_version: era.default_protocol_version().to_string(),
            supported_versions: vec![era.default_protocol_version().to_string()],
            capabilities: Default::default(),
            server_info: None,
            extensions: Default::default(),
            instructions: None,
        },
    )
}

#[tokio::test]
async fn a_modern_server_is_answered_by_retrying_the_request() {
    let (transport, log) = modern_server();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = settled(transport, ProtocolEra::Modern).await;

    let text = open_pull_request(client, Arc::new(Reviewer { seen: seen.clone() })).await;
    assert_eq!(text, final_text());

    // The handler was asked exactly once, with the server's own wording.
    assert_eq!(
        *seen.lock().expect("handler mutex"),
        vec!["Please provide your GitHub username".to_string()]
    );

    let journal = log.lock().expect("journal mutex");
    assert_eq!(journal.call_ids.len(), 2, "the call was sent twice");

    // "The JSON-RPC id MUST be different between the initial request and the
    // retry, as they are independent requests."
    assert_ne!(
        journal.call_ids[0], journal.call_ids[1],
        "the retry reused the initial request's id"
    );

    // The first attempt carried neither; the retry carried both.
    assert_eq!(journal.echoed_state[0], None);
    assert_eq!(journal.responses[0], None);

    // "Clients MUST echo back the exact value of that field."
    assert_eq!(journal.echoed_state[1], Some(json!(REQUEST_STATE)));
    assert_eq!(
        journal.responses[1],
        Some(json!({
            ELICIT_KEY: {"action": "accept", "content": {ELICIT_FIELD: ELICIT_ANSWER}},
        }))
    );
}

#[tokio::test]
async fn a_legacy_server_is_answered_by_replying_to_its_pushed_request() {
    let (transport, log) = legacy_server();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let client = settled(transport, ProtocolEra::Legacy).await;

    let text = open_pull_request(client, Arc::new(Reviewer { seen: seen.clone() })).await;

    // Identical caller code, identical outcome — the phase gate.
    assert_eq!(text, final_text());
    assert_eq!(
        *seen.lock().expect("handler mutex"),
        vec!["Please provide your GitHub username".to_string()]
    );

    let journal = log.lock().expect("journal mutex");
    // One call, not two: the legacy server held it open rather than asking the
    // client to come back.
    assert_eq!(journal.call_ids.len(), 1);
    assert_eq!(
        journal.responses,
        vec![Some(
            json!({"action": "accept", "content": {ELICIT_FIELD: ELICIT_ANSWER}})
        )],
        "the pushed request was answered with the same ElicitResult"
    );
}

#[tokio::test]
async fn without_a_handler_the_modern_path_says_so() {
    let (transport, _log) = modern_server();
    let client = settled(transport, ProtocolEra::Modern).await;

    // No handler set. A missing-field decode failure would be a terrible way
    // to learn that the server wanted to ask the user something.
    let error = client
        .call_tool(TOOL_NAME, json!({}))
        .await
        .expect_err("must not silently fail to decode");
    let message = error.to_string();
    assert!(
        message.contains("input handler"),
        "unhelpful error: {message}"
    );
}
