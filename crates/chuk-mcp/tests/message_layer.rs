//! Coverage-oriented tests for the message layer, driven through the public
//! API against an in-memory fake transport.

use serde_json::{json, Value};
use tokio::sync::mpsc;

use chuk_mcp::protocol::json_rpc::{
    create_error_response, create_response, JsonRpcMessage, RequestId,
};
use chuk_mcp::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};

struct Fake {
    read: ReadStream,
    write: WriteStream,
    inject: mpsc::Sender<JsonRpcMessage>,
    sent: mpsc::Receiver<JsonRpcMessage>,
}

impl Fake {
    fn new() -> Self {
        let (inject, read) = message_channel(16);
        let (write, sent) = mpsc::channel(16);
        Fake {
            read,
            write,
            inject,
            sent,
        }
    }
    fn streams(&self) -> (ReadStream, WriteStream) {
        (self.read.clone(), self.write.clone())
    }
    async fn recv(&mut self) -> JsonRpcMessage {
        self.sent.recv().await.expect("client sent a message")
    }
    async fn recv_id(&mut self) -> RequestId {
        self.recv().await.id().expect("has id").clone()
    }
    async fn respond(&self, id: RequestId, result: Value) {
        self.inject
            .send(JsonRpcMessage::Response(create_response(id, Some(result))))
            .await
            .unwrap();
    }
    async fn respond_error(&self, id: RequestId, code: i64, msg: &str) {
        self.inject
            .send(JsonRpcMessage::Error(create_error_response(
                id, code, msg, None,
            )))
            .await
            .unwrap();
    }
}

// --- tools ---------------------------------------------------------------

#[tokio::test]
async fn tools_list_and_call() {
    use chuk_mcp::protocol::messages::tools::{
        is_tools_list_changed_notification, send_tools_call, send_tools_list,
    };
    let mut fake = Fake::new();

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_tools_list(&r, &w, Some("cur")).await });
    let req = fake.recv().await;
    assert_eq!(req.params().unwrap()["cursor"], "cur");
    fake.respond(
        req.id().unwrap().clone(),
        json!({"tools": [{"name": "t", "inputSchema": {}}], "nextCursor": "n"}),
    )
    .await;
    let result = h.await.unwrap().unwrap();
    assert_eq!(result.tools[0].name, "t");
    assert_eq!(result.next_cursor.as_deref(), Some("n"));

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_tools_call(&r, &w, "t", json!({"a": 1})).await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({"content": [{"type": "text", "text": "ok"}]}))
        .await;
    assert_eq!(h.await.unwrap().unwrap().text(), "ok");

    // non-object arguments -> validation error
    let (r, w) = fake.streams();
    assert!(send_tools_call(&r, &w, "t", json!(5)).await.is_err());

    let notif = JsonRpcMessage::Notification(chuk_mcp::protocol::json_rpc::create_notification(
        "notifications/tools/list_changed",
        None,
    ));
    assert!(is_tools_list_changed_notification(&notif));
}

// --- resources -----------------------------------------------------------

#[tokio::test]
async fn resources_full() {
    use chuk_mcp::protocol::messages::resources::{
        is_resources_list_changed_notification, parse_resources_updated_notification,
        send_resources_list, send_resources_read, send_resources_subscribe,
        send_resources_templates_list, send_resources_unsubscribe,
    };
    let mut fake = Fake::new();

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_resources_list(&r, &w, None).await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({"resources": [{"uri": "u", "name": "n"}]}))
        .await;
    assert_eq!(h.await.unwrap().unwrap().resources[0].uri, "u");

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_resources_read(&r, &w, "u").await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({"contents": [{"uri": "u", "text": "body"}]}))
        .await;
    assert_eq!(
        h.await.unwrap().unwrap().contents[0].text.as_deref(),
        Some("body")
    );

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_resources_templates_list(&r, &w).await });
    let id = fake.recv_id().await;
    fake.respond(
        id,
        json!({"resourceTemplates": [{"uriTemplate": "u/{x}", "name": "n"}]}),
    )
    .await;
    assert_eq!(
        h.await.unwrap().unwrap().resource_templates[0].uri_template,
        "u/{x}"
    );

    // subscribe / unsubscribe: success -> true
    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_resources_subscribe(&r, &w, "u").await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({})).await;
    assert!(h.await.unwrap());

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_resources_unsubscribe(&r, &w, "u").await });
    let id = fake.recv_id().await;
    fake.respond_error(id, -32603, "boom").await;
    assert!(!h.await.unwrap()); // error swallowed -> false

    // notifications
    assert!(is_resources_list_changed_notification(
        &JsonRpcMessage::Notification(chuk_mcp::protocol::json_rpc::create_notification(
            "notifications/resources/list_changed",
            None
        ))
    ));
    let updated = JsonRpcMessage::Notification(chuk_mcp::protocol::json_rpc::create_notification(
        "notifications/resources/updated",
        Some(json!({"uri": "u"})),
    ));
    assert_eq!(
        parse_resources_updated_notification(&updated).as_deref(),
        Some("u")
    );
}

// --- prompts -------------------------------------------------------------

#[tokio::test]
async fn prompts_full() {
    use chuk_mcp::protocol::messages::prompts::{
        is_prompts_list_changed_notification, send_prompts_get, send_prompts_list,
    };
    let mut fake = Fake::new();

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_prompts_list(&r, &w, None).await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({"prompts": [{"name": "p"}]})).await;
    assert_eq!(h.await.unwrap().unwrap().prompts[0].name, "p");

    let (r, w) = fake.streams();
    let h =
        tokio::spawn(
            async move { send_prompts_get(&r, &w, "p", Some(json!({"topic": "x"}))).await },
        );
    let req = fake.recv().await;
    assert_eq!(req.params().unwrap()["arguments"]["topic"], "x");
    fake.respond(
        req.id().unwrap().clone(),
        json!({"messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}]}),
    )
    .await;
    assert_eq!(h.await.unwrap().unwrap().messages.unwrap()[0].role, "user");

    // non-object args -> validation error
    let (r, w) = fake.streams();
    assert!(send_prompts_get(&r, &w, "p", Some(json!("bad")))
        .await
        .is_err());

    assert!(is_prompts_list_changed_notification(
        &JsonRpcMessage::Notification(chuk_mcp::protocol::json_rpc::create_notification(
            "notifications/prompts/list_changed",
            None
        ))
    ));
}

// --- ping ----------------------------------------------------------------

#[tokio::test]
async fn ping_success_and_failure() {
    use chuk_mcp::protocol::messages::ping::send_ping;
    let mut fake = Fake::new();

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_ping(&r, &w).await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({})).await;
    assert!(h.await.unwrap());

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_ping(&r, &w).await });
    let id = fake.recv_id().await;
    fake.respond_error(id, -32601, "no").await;
    assert!(!h.await.unwrap());
}

// --- logging -------------------------------------------------------------

#[tokio::test]
async fn logging_set_level() {
    use chuk_mcp::protocol::messages::logging::{send_logging_set_level, LogLevel};
    let mut fake = Fake::new();
    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_logging_set_level(&r, &w, LogLevel::Warning).await });
    let req = fake.recv().await;
    assert_eq!(req.params().unwrap()["level"], "warning");
    fake.respond(req.id().unwrap().clone(), json!({})).await;
    assert!(h.await.unwrap().is_ok());
    // exercise all level string mappings
    for lvl in [
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Notice,
        LogLevel::Warning,
        LogLevel::Error,
        LogLevel::Critical,
        LogLevel::Alert,
        LogLevel::Emergency,
    ] {
        assert!(!lvl.as_str().is_empty());
    }
}

// --- completions ---------------------------------------------------------

#[tokio::test]
async fn completions_full() {
    use chuk_mcp::protocol::messages::completions::{
        complete_enum_value, complete_prompt_argument, complete_resource_argument,
        send_completion_complete, ArgumentInfo, Reference,
    };
    let mut fake = Fake::new();

    let reference = Reference::Prompt {
        name: "p".to_string(),
    };
    let arg = ArgumentInfo {
        name: "lang".to_string(),
        value: "py".to_string(),
        extra: Default::default(),
    };
    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_completion_complete(&r, &w, &reference, &arg).await });
    let id = fake.recv_id().await;
    fake.respond(
        id,
        json!({"completion": {"values": ["python"], "total": 1}}),
    )
    .await;
    let result = h.await.unwrap().unwrap();
    assert_eq!(result.values, vec!["python"]);

    let (r, w) = fake.streams();
    let h = tokio::spawn(
        async move { complete_resource_argument(&r, &w, "file:///x", "a", "b").await },
    );
    let id = fake.recv_id().await;
    fake.respond(id, json!({"completion": {"values": []}}))
        .await;
    assert!(h.await.unwrap().unwrap().values.is_empty());

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { complete_prompt_argument(&r, &w, "p", "a", "b").await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({"completion": {"values": ["x"]}}))
        .await;
    assert_eq!(h.await.unwrap().unwrap().values, vec!["x"]);

    assert_eq!(
        complete_enum_value("p", &["python", "perl", "rust"], false),
        vec!["python", "perl"]
    );
    assert_eq!(
        complete_enum_value("P", &["python", "Perl"], true),
        vec!["Perl"]
    );
}

// --- roots ---------------------------------------------------------------

#[tokio::test]
async fn roots_full() {
    use chuk_mcp::protocol::messages::roots::{
        create_file_root, handle_roots_list_request, parse_file_root, send_roots_list, Root,
        RootsManager,
    };
    let mut fake = Fake::new();

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_roots_list(&r, &w).await });
    let id = fake.recv_id().await;
    fake.respond(id, json!({"roots": [{"uri": "file:///a"}]}))
        .await;
    assert_eq!(h.await.unwrap().unwrap().roots[0].uri, "file:///a");

    let root = create_file_root(std::path::Path::new("/tmp/x"), Some("x".into())).unwrap();
    assert_eq!(
        parse_file_root(&root).unwrap(),
        std::path::PathBuf::from("/tmp/x")
    );
    assert!(Root::new("http://no", None).is_err());
    assert!(parse_file_root(&Root {
        uri: "http://no".into(),
        name: None,
        extra: Default::default(),
    })
    .is_err());

    let resp = handle_roots_list_request(std::slice::from_ref(&root), RequestId::Num(1));
    assert!(resp.is_response());

    let mut mgr = RootsManager::new(None);
    mgr.add_root(root.clone()).await;
    assert_eq!(mgr.get_roots().len(), 1);
    mgr.remove_root(&root.uri).await;
    assert!(mgr.get_roots().is_empty());
    mgr.add_root(root).await;
    mgr.clear().await;
    assert!(mgr.get_roots().is_empty());
    assert!(mgr.handle_list_request(RequestId::Num(2)).is_response());
}

// --- sampling ------------------------------------------------------------

#[tokio::test]
async fn sampling_full() {
    use chuk_mcp::protocol::messages::sampling::{
        create_model_preferences, create_sampling_message, sample_text,
        send_sampling_create_message, IncludeContext, SamplingOptions,
    };
    use chuk_mcp::protocol::types::content::Role;
    let mut fake = Fake::new();

    let messages = vec![create_sampling_message(Role::User, "hi")];
    let options = SamplingOptions {
        model_preferences: Some(create_model_preferences(
            Some(vec!["claude".into()]),
            Some(0.1),
            Some(0.2),
            Some(0.3),
        )),
        system_prompt: Some("sys".into()),
        include_context: Some(IncludeContext::ThisServer),
        temperature: Some(0.5),
        stop_sequences: Some(vec!["stop".into()]),
        metadata: Some(serde_json::Map::new()),
    };
    let (r, w) = fake.streams();
    let h =
        tokio::spawn(
            async move { send_sampling_create_message(&r, &w, &messages, 100, options).await },
        );
    let req = fake.recv().await;
    assert_eq!(req.params().unwrap()["maxTokens"], 100);
    assert_eq!(req.params().unwrap()["systemPrompt"], "sys");
    fake.respond(
        req.id().unwrap().clone(),
        json!({"role": "assistant", "content": {"type": "text", "text": "hello"}, "model": "m"}),
    )
    .await;
    assert!(h.await.unwrap().is_ok());

    let (r, w) = fake.streams();
    let h = tokio::spawn(async move {
        sample_text(&r, &w, "prompt", 50, Some("claude"), Some(0.7), Some("sys")).await
    });
    let id = fake.recv_id().await;
    fake.respond(
        id,
        json!({"role": "assistant", "content": {"type": "text", "text": "hi"}, "model": "m", "stopReason": "endTurn"}),
    )
    .await;
    let result = h.await.unwrap().unwrap();
    assert_eq!(result.model, "m");
}

// --- initialize ----------------------------------------------------------

#[tokio::test]
async fn initialize_paths() {
    use chuk_mcp::protocol::messages::initialize::{
        get_current_version, get_supported_versions, send_initialize, send_initialize_with_options,
        InitializeOptions,
    };
    use chuk_mcp::protocol::types::capabilities::ClientCapabilities;
    use chuk_mcp::protocol::types::info::ClientInfo;

    assert!(!get_supported_versions().is_empty());
    assert!(!get_current_version().is_empty());

    // accepted counter-proposal: server returns a different-but-supported version
    let mut fake = Fake::new();
    let (r, w) = fake.streams();
    let opts = InitializeOptions {
        preferred_version: Some("2025-06-18".into()),
        client_info: Some(ClientInfo::default()),
        capabilities: Some(ClientCapabilities::default()),
        ..Default::default()
    };
    let h = tokio::spawn(async move { send_initialize_with_options(&r, &w, opts).await });
    let req = fake.recv().await;
    assert_eq!(req.params().unwrap()["protocolVersion"], "2025-06-18");
    fake.respond(
        req.id().unwrap().clone(),
        json!({"protocolVersion": "2025-03-26", "serverInfo": {"name": "s", "version": "1"}, "capabilities": {}}),
    )
    .await;
    assert_eq!(h.await.unwrap().unwrap().protocol_version, "2025-03-26");

    // INVALID_PARAMS mentioning "protocol version" -> VersionMismatch
    let mut fake = Fake::new();
    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_initialize(&r, &w).await });
    let id = fake.recv_id().await;
    fake.respond_error(id, -32602, "unsupported protocol version")
        .await;
    assert!(matches!(
        h.await.unwrap().unwrap_err(),
        chuk_mcp::McpError::VersionMismatch { .. }
    ));

    // other INVALID_PARAMS passes through unchanged
    let mut fake = Fake::new();
    let (r, w) = fake.streams();
    let h = tokio::spawn(async move { send_initialize(&r, &w).await });
    let id = fake.recv_id().await;
    fake.respond_error(id, -32602, "bad params").await;
    assert!(!matches!(
        h.await.unwrap().unwrap_err(),
        chuk_mcp::McpError::VersionMismatch { .. }
    ));
}

// --- notifications -------------------------------------------------------

#[tokio::test]
async fn notifications_full() {
    use chuk_mcp::protocol::messages::notifications::{
        parse_cancelled_notification, parse_logging_message_notification,
        parse_progress_notification, send_cancelled_notification, send_progress_notification,
        send_roots_list_changed_notification, NotificationHandler,
    };
    let mut fake = Fake::new();
    let (_, w) = fake.streams();

    send_progress_notification(&w, RequestId::Str("t".into()), 0.5, Some(1.0), Some("half"))
        .await
        .unwrap();
    let msg = fake.recv().await;
    let p = parse_progress_notification(&msg).unwrap();
    assert_eq!(p.progress, 0.5);
    assert_eq!(p.total, Some(1.0));

    send_cancelled_notification(&w, RequestId::Num(3), Some("stop"))
        .await
        .unwrap();
    let msg = fake.recv().await;
    let c = parse_cancelled_notification(&msg).unwrap();
    assert_eq!(c.reason.as_deref(), Some("stop"));

    send_roots_list_changed_notification(&w).await.unwrap();
    let msg = fake.recv().await;
    assert_eq!(msg.method(), Some("notifications/roots/list_changed"));

    // logging notification parsing
    let log = JsonRpcMessage::Notification(chuk_mcp::protocol::json_rpc::create_notification(
        "notifications/message",
        Some(json!({"level": "error", "data": "boom", "logger": "x"})),
    ));
    let parsed = parse_logging_message_notification(&log).unwrap();
    assert_eq!(parsed.level, "error");
    // non-matching messages parse to None
    assert!(parse_progress_notification(&log).is_none());

    // router
    let mut handler = NotificationHandler::new();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h2 = hits.clone();
    handler.register(
        "notifications/message",
        Box::new(move |_| {
            h2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }),
    );
    handler.handle(&log);
    handler.handle(&JsonRpcMessage::Notification(
        chuk_mcp::protocol::json_rpc::create_notification("unregistered", None),
    ));
    assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
}
