//! What the server does with each kind of message.

use super::*;
use crate::protocol::json_rpc::parse_message;
use crate::protocol::types::errors::INVALID_PARAMS;
use serde_json::json;

fn server() -> McpServer {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_tool(
        "greet",
        json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        "Say hello",
        |args| async move {
            let name = args.get("name").and_then(Value::as_str).unwrap_or("world");
            Ok(json!(format!("Hello, {name}!")))
        },
    );
    server.register_resource(
        "demo://greeting",
        "greeting",
        "A demo resource",
        "text/plain",
        || async { Ok("hi there".to_string()) },
    );
    server
}

/// Ask `server` one request and unwrap the result it answered with.
async fn result_of(server: &McpServer, method: &str, params: Value) -> Value {
    let message = parse_message(&json!({
        "jsonrpc": "2.0", "id": 1, "method": method, "params": params
    }))
    .unwrap();
    let (response, _) = server.handle_message(message, None).await;
    response
        .expect("a request is answered")
        .result()
        .expect("a result, not an error")
        .clone()
}

/// Ask `server` one request and unwrap the error code it refused with.
async fn error_of(server: &McpServer, method: &str, params: Value) -> i64 {
    let message = parse_message(&json!({
        "jsonrpc": "2.0", "id": 2, "method": method, "params": params
    }))
    .unwrap();
    let (response, _) = server.handle_message(message, None).await;
    response
        .expect("a request is answered")
        .error()
        .expect("an error, not a result")
        .code
}

#[tokio::test]
async fn tools_roundtrip() {
    let server = server();

    let listed = result_of(&server, MessageMethod::TOOLS_LIST, json!({})).await;
    assert_eq!(listed["tools"][0]["name"], json!("greet"));

    let called = result_of(
        &server,
        MessageMethod::TOOLS_CALL,
        json!({"name": "greet", "arguments": {"name": "Rust"}}),
    )
    .await;
    assert_eq!(called["content"][0]["text"], json!("Hello, Rust!"));

    assert_eq!(
        error_of(
            &server,
            MessageMethod::TOOLS_CALL,
            json!({"name": "missing", "arguments": {}})
        )
        .await,
        INVALID_PARAMS
    );
}

#[tokio::test]
async fn resources_roundtrip() {
    let server = server();

    let listed = result_of(&server, MessageMethod::RESOURCES_LIST, json!({})).await;
    assert_eq!(listed["resources"][0]["uri"], json!("demo://greeting"));

    let read = result_of(
        &server,
        MessageMethod::RESOURCES_READ,
        json!({"uri": "demo://greeting"}),
    )
    .await;
    assert_eq!(read["contents"][0]["text"], json!("hi there"));

    assert_eq!(
        error_of(
            &server,
            MessageMethod::RESOURCES_READ,
            json!({"uri": "demo://nothing"})
        )
        .await,
        INVALID_PARAMS
    );
}

#[tokio::test]
async fn a_binary_resource_and_a_template_are_reachable_through_dispatch() {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_binary_resource("t://bytes", "", "", "image/png", || async {
        Ok("aGk=".to_string())
    });
    server.register_resource_template("t://item/{id}", "", "", "", |bound| async move {
        Ok(bound["id"].clone())
    });

    let bytes = result_of(
        &server,
        MessageMethod::RESOURCES_READ,
        json!({"uri": "t://bytes"}),
    )
    .await;
    assert_eq!(bytes["contents"][0]["blob"], json!("aGk="));

    let templated = result_of(
        &server,
        MessageMethod::RESOURCES_READ,
        json!({"uri": "t://item/42"}),
    )
    .await;
    assert_eq!(templated["contents"][0]["text"], json!("42"));

    let listed = result_of(&server, MessageMethod::RESOURCES_TEMPLATES_LIST, json!({})).await;
    assert_eq!(
        listed["resourceTemplates"][0]["uriTemplate"],
        json!("t://item/{id}")
    );
}

#[tokio::test]
async fn subscribing_records_the_uri_and_unsubscribing_forgets_it() {
    let server = server();

    assert_eq!(
        result_of(
            &server,
            MessageMethod::RESOURCES_SUBSCRIBE,
            json!({"uri": "t://watched"})
        )
        .await,
        json!({})
    );
    assert_eq!(server.subscriptions(), vec!["t://watched".to_string()]);

    result_of(
        &server,
        MessageMethod::RESOURCES_UNSUBSCRIBE,
        json!({"uri": "t://watched"}),
    )
    .await;
    assert!(server.subscriptions().is_empty());

    // A subscription to nothing in particular is a malformed request.
    assert_eq!(
        error_of(&server, MessageMethod::RESOURCES_SUBSCRIBE, json!({})).await,
        INVALID_PARAMS
    );
}

#[tokio::test]
async fn setting_the_log_level_takes_and_rejects_what_it_should() {
    let server = server();
    assert_eq!(server.log_level(), logging::DEFAULT_LEVEL);

    assert_eq!(
        result_of(
            &server,
            MessageMethod::LOGGING_SET_LEVEL,
            json!({"level": "debug"})
        )
        .await,
        json!({})
    );
    assert_eq!(server.log_level(), LogLevel::Debug);

    // An unknown severity is refused rather than silently stored, or the
    // client would believe a filter is in place that is not.
    assert_eq!(
        error_of(
            &server,
            MessageMethod::LOGGING_SET_LEVEL,
            json!({"level": "chatty"})
        )
        .await,
        INVALID_PARAMS
    );
    assert_eq!(server.log_level(), LogLevel::Debug);
}

#[tokio::test]
async fn completion_answers_even_with_nothing_to_suggest() {
    let answer = result_of(&server(), MessageMethod::COMPLETION_COMPLETE, json!({})).await;
    assert_eq!(answer["completion"]["values"], json!([]));

    let mut server = server();
    server
        .register_completion(|_reference, _name, value| async move { vec![format!("{value}is")] });
    let answer = result_of(
        &server,
        MessageMethod::COMPLETION_COMPLETE,
        json!({"argument": {"name": "city", "value": "par"}}),
    )
    .await;
    assert_eq!(answer["completion"]["values"], json!(["paris"]));
}

/// A tool that needs more input must reach the client as an
/// `input_required` it can answer, not as JSON pretty-printed into a text
/// block that reads like a finished answer.
#[tokio::test]
async fn input_required_is_returned_unwrapped() {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_tool("ask", json!({"type": "object"}), "Needs input", |_| async {
        Ok(json!({
            "resultType": "input_required",
            "inputRequests": {},
            "requestState": "state-1",
        }))
    });

    let result = result_of(
        &server,
        MessageMethod::TOOLS_CALL,
        json!({"name": "ask", "arguments": {}}),
    )
    .await;

    assert_eq!(result["resultType"], json!("input_required"));
    assert!(
        result.get("content").is_none(),
        "an input_required must not be wrapped in content blocks: {result}"
    );

    let parsed = crate::protocol::mrtr::InputRequired::from_result(&result)
        .expect("a well-formed input_required")
        .expect("recognised as an input_required");
    assert_eq!(
        parsed.request_state,
        Some(crate::protocol::mrtr::RequestState::new("state-1"))
    );
}

/// A tool that fails has answered the call. Only an invalid call — an
/// unknown tool — is a JSON-RPC error.
#[tokio::test]
async fn a_failing_tool_answers_with_is_error() {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_tool("boom", json!({"type": "object"}), "Fails", |_| async {
        Err("the kettle is empty".to_string())
    });

    let result = result_of(
        &server,
        MessageMethod::TOOLS_CALL,
        json!({"name": "boom", "arguments": {}}),
    )
    .await;
    assert_eq!(result["isError"], json!(true));
    assert_eq!(result["content"][0]["text"], json!("the kettle is empty"));
}

#[tokio::test]
async fn only_a_call_to_an_interactive_tool_needs_a_stream() {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_tool("plain", json!({}), "", |_| async { Ok(json!("done")) });
    server.register_interactive_tool("talks", json!({}), "", |_, _| async { Ok(json!("done")) });

    let call = |name: &str| {
        parse_message(&json!({
            "jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_CALL,
            "params": {"name": name}
        }))
        .unwrap()
    };

    assert!(server.needs_stream(&call("talks")));
    assert!(!server.needs_stream(&call("plain")));
    assert!(!server.needs_stream(&call("missing")));

    // Nothing else needs one, whatever it is.
    let listing =
        parse_message(&json!({"jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_LIST}))
            .unwrap();
    assert!(!server.needs_stream(&listing));
}

#[tokio::test]
async fn an_interactive_tool_reaches_the_client_and_is_answered() {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_interactive_tool("ask", json!({}), "", |_, context: CallContext| async move {
        context.log(LogLevel::Info.as_str(), json!("starting"));
        let answer = context.sample(json!({"maxTokens": 10})).await?;
        Ok(json!(format!(
            "said: {}",
            answer["text"].as_str().unwrap_or("")
        )))
    });

    let (sender, mut outbound) = tokio::sync::mpsc::unbounded_channel();
    let call = parse_message(&json!({
        "jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_CALL,
        "params": {"name": "ask", "arguments": {}, "_meta": {"progressToken": "p1"}}
    }))
    .unwrap();
    let context = server.context_for(&call, sender);

    let pending = server.pending();
    let answering = tokio::spawn(async move {
        // The log arrives first, then the request this must answer.
        let logged = outbound.recv().await.expect("a notification");
        assert_eq!(logged.method(), Some(MessageMethod::NOTIFICATION_MESSAGE));

        let asked = outbound.recv().await.expect("a request");
        assert_eq!(asked.method(), Some(MessageMethod::SAMPLING_CREATE_MESSAGE));
        pending.resolve(&JsonRpcMessage::Response(
            crate::protocol::json_rpc::create_response(
                asked.id().expect("an id").clone(),
                Some(json!({"text": "hello"})),
            ),
        ));
    });

    let (response, _) = server.handle_message_with(call, None, context).await;
    answering.await.expect("the client answered");

    let result = response
        .expect("answered")
        .result()
        .expect("a result")
        .clone();
    assert_eq!(result["content"][0]["text"], json!("said: hello"));
}

#[tokio::test]
async fn a_progress_token_reaches_the_tool_that_reports_against_it() {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_interactive_tool(
        "slow",
        json!({}),
        "",
        |_, context: CallContext| async move {
            context.progress(100.0, Some(100.0));
            Ok(json!("done"))
        },
    );

    let (sender, mut outbound) = tokio::sync::mpsc::unbounded_channel();
    let call = parse_message(&json!({
        "jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_CALL,
        "params": {"name": "slow", "_meta": {"progressToken": "p1"}}
    }))
    .unwrap();
    let context = server.context_for(&call, sender);

    server.handle_message_with(call, None, context).await;

    let sent = outbound.recv().await.expect("a progress notification");
    assert_eq!(sent.method(), Some(MessageMethod::NOTIFICATION_PROGRESS));
    assert_eq!(sent.params().unwrap()["progressToken"], json!("p1"));
}

/// An answer to a server request is delivered, not dispatched — and one
/// nobody is waiting for is dropped rather than mistaken for a request.
#[tokio::test]
async fn an_answer_from_the_client_is_routed_to_whoever_waits() {
    let server = server();
    let (id, receiver) = server.pending().issue();

    let answer = JsonRpcMessage::Response(crate::protocol::json_rpc::create_response(
        id,
        Some(json!({"ok": true})),
    ));
    let (response, _) = server.handle_message(answer, None).await;
    assert!(response.is_none(), "an answer earns no reply");
    assert_eq!(receiver.await.expect("delivered"), Ok(json!({"ok": true})));

    let stray = JsonRpcMessage::Response(crate::protocol::json_rpc::create_response(
        crate::protocol::json_rpc::RequestId::Str("srv-404".into()),
        Some(json!({})),
    ));
    let (response, _) = server.handle_message(stray, None).await;
    assert!(response.is_none());
}

#[tokio::test]
async fn a_notification_earns_no_reply() {
    let server = server();
    let notification = parse_message(&json!({
        "jsonrpc": "2.0", "method": MessageMethod::NOTIFICATION_INITIALIZED
    }))
    .unwrap();

    let (response, _) = server.handle_message(notification, None).await;
    assert!(response.is_none());
}

/// Every registration entry point reaches its registry, including the ones a
/// conformance fixture uses and nothing else would.
#[tokio::test]
async fn each_registration_helper_reaches_its_registry() {
    let mut server = McpServer::new("test-server", "1.0.0", None)
        .with_instructions("how to use this")
        .with_max_buffer_size(4096)
        .with_client_timeout(Duration::from_millis(10));

    server.register_binary_resource_template(
        "t://pixels/{id}",
        "pixels",
        "Bytes by id",
        "image/png",
        |bound| async move { Ok(format!("aGk{}=", bound["id"])) },
    );
    server.register_prompt("bare", "Nothing to fill in", vec![], |_| async {
        Ok(vec![prompts::text_message("user", "hello")])
    });

    let templated = result_of(
        &server,
        MessageMethod::RESOURCES_READ,
        json!({"uri": "t://pixels/7"}),
    )
    .await;
    assert_eq!(templated["contents"][0]["blob"], json!("aGk7="));
    assert_eq!(templated["contents"][0]["mimeType"], json!("image/png"));

    let prompts = result_of(&server, MessageMethod::PROMPTS_LIST, json!({})).await;
    assert_eq!(prompts["prompts"][0]["name"], json!("bare"));

    // The instructions given are the ones `server/discover` reports.
    let discovered = result_of(&server, MessageMethod::SERVER_DISCOVER, json!({})).await;
    assert_eq!(discovered["instructions"], json!("how to use this"));
}

/// A resource whose handler fails is a fault, not an absence: the client is
/// told why rather than that the resource does not exist.
#[tokio::test]
async fn a_resource_that_fails_to_read_says_so() {
    let mut server = McpServer::new("test-server", "1.0.0", None);
    server.register_resource("t://broken", "", "", "", || async {
        Err("the disk is gone".to_string())
    });

    let message = parse_message(&json!({
        "jsonrpc": "2.0", "id": 1, "method": MessageMethod::RESOURCES_READ,
        "params": {"uri": "t://broken"}
    }))
    .unwrap();
    let (response, _) = server.handle_message(message, None).await;
    let error = response.unwrap().error().unwrap().clone();

    assert_eq!(error.code, crate::protocol::types::errors::INTERNAL_ERROR);
    assert!(error.message.contains("the disk is gone"));
}

/// A prompt that cannot be rendered is refused by name.
#[tokio::test]
async fn an_unknown_prompt_is_refused() {
    let message = parse_message(&json!({
        "jsonrpc": "2.0", "id": 1, "method": MessageMethod::PROMPTS_GET,
        "params": {"name": "nope"}
    }))
    .unwrap();
    let (response, _) = server().handle_message(message, None).await;
    assert_eq!(response.unwrap().error().unwrap().code, INVALID_PARAMS);
}

/// A request with no id cannot be answered, whatever it asks for — there is
/// nowhere to send the answer.
#[tokio::test]
async fn a_request_without_an_id_is_not_answered() {
    for method in [
        MessageMethod::TOOLS_LIST,
        MessageMethod::RESOURCES_LIST,
        MessageMethod::RESOURCES_TEMPLATES_LIST,
        MessageMethod::PROMPTS_LIST,
        MessageMethod::SERVER_DISCOVER,
    ] {
        let notification = parse_message(&json!({"jsonrpc": "2.0", "method": method})).unwrap();
        let (response, _) = server().handle_message(notification, None).await;
        assert!(response.is_none(), "{method} answered a notification");
    }
}
