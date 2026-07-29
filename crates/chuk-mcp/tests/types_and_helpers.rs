//! Coverage-oriented tests for the pure type helpers, errors, versioning and
//! batching.

use serde_json::json;

// --- content -------------------------------------------------------------

#[test]
fn content_helpers_and_parsing() {
    use chuk_mcp::protocol::types::content::{
        create_annotations, create_audio_content, create_embedded_text_resource,
        create_image_content, create_text_content, parse_content, Content, Role,
    };

    let text = create_text_content(
        "hi",
        Some(create_annotations(Some(vec![Role::User]), Some(0.5))),
    );
    assert!(text.is_text());
    assert_eq!(text.as_text(), Some("hi"));

    let image = create_image_content("QQ==", "image/png", None);
    assert!(image.is_image());
    assert!(image.as_text().is_none());

    let audio = create_audio_content("QQ==", "audio/wav", None);
    assert!(audio.is_audio());

    let embedded =
        create_embedded_text_resource("file:///x", "body", Some("text/plain".into()), None);
    assert!(embedded.is_embedded_resource());

    // round-trip each through JSON via parse_content
    for c in [&text, &image, &audio, &embedded] {
        let value = serde_json::to_value(c).unwrap();
        let parsed = parse_content(&value).unwrap();
        assert_eq!(&parsed, c);
    }

    // unknown type -> error
    assert!(parse_content(&json!({"type": "nope"})).is_err());
    let _ = Content::Text {
        text: "x".into(),
        annotations: None,
    };
}

// --- elicitation ---------------------------------------------------------

#[test]
fn elicitation_builders() {
    use chuk_mcp::protocol::types::elicitation::{
        create_choice_elicitation, create_confirmation_elicitation, create_form_elicitation,
        create_text_input_elicitation,
    };

    let text = create_text_input_elicitation("msg", "field", Some("Title".into()), true);
    assert_eq!(text.schema["required"], json!(["field"]));
    let optional = create_text_input_elicitation("msg", "field", None, false);
    assert!(optional.schema.get("required").is_none());

    let choice = create_choice_elicitation("pick", &["a", "b"], "c", None);
    assert_eq!(choice.schema["properties"]["c"]["enum"], json!(["a", "b"]));

    let confirm = create_confirmation_elicitation("ok?", "yes", None);
    assert_eq!(confirm.schema["properties"]["yes"]["type"], "boolean");

    let mut fields = serde_json::Map::new();
    fields.insert("name".into(), json!({"type": "string"}));
    let form = create_form_elicitation(
        "form",
        fields.clone(),
        Some(vec!["name".into()]),
        Some("T".into()),
    );
    assert_eq!(form.schema["required"], json!(["name"]));
    let form2 = create_form_elicitation("form", fields, None, None);
    assert!(form2.schema.get("required").is_none());
}

// --- errors --------------------------------------------------------------

#[test]
fn error_helpers_and_type() {
    use chuk_mcp::protocol::types::errors::*;

    for code in [
        PARSE_ERROR,
        INVALID_REQUEST,
        METHOD_NOT_FOUND,
        INVALID_PARAMS,
        INTERNAL_ERROR,
        CONNECTION_CLOSED,
        REQUEST_TIMEOUT,
        MCP_INITIALIZATION_FAILED,
        MCP_CAPABILITY_NOT_SUPPORTED,
        MCP_RESOURCE_NOT_FOUND,
        MCP_TOOL_NOT_FOUND,
        MCP_PROMPT_NOT_FOUND,
        MCP_AUTHORIZATION_FAILED,
        MCP_PROTOCOL_VERSION_MISMATCH,
        HEADER_MISMATCH,
        MISSING_REQUIRED_CLIENT_CAPABILITY,
        UNSUPPORTED_PROTOCOL_VERSION,
    ] {
        assert!(!get_error_message(code).is_empty());
        // No code may fall through to the unknown-code formatter.
        assert!(!get_error_message(code).contains("Unknown"));
    }
    assert!(get_error_message(12345).contains("Unknown"));

    assert!(is_retryable_error(INTERNAL_ERROR));
    assert!(!is_retryable_error(METHOD_NOT_FOUND));
    assert!(is_server_error(-32050));
    assert!(!is_server_error(-1));
    assert!(is_standard_jsonrpc_error(PARSE_ERROR));
    assert!(!is_standard_jsonrpc_error(CONNECTION_CLOSED));
    assert!(is_mcp_specific_error(MCP_TOOL_NOT_FOUND));
    assert!(!is_mcp_specific_error(PARSE_ERROR));

    // McpError variants
    let retry = McpError::from_json_rpc(INTERNAL_ERROR, "boom", Some(json!({"x": 1})));
    assert!(retry.is_retryable());
    assert_eq!(retry.code(), Some(INTERNAL_ERROR));
    assert_eq!(retry.to_error_object().code, INTERNAL_ERROR);

    let non = McpError::from_json_rpc(METHOD_NOT_FOUND, "no", None);
    assert!(!non.is_retryable());

    let proto = McpError::protocol(PARSE_ERROR, "bad");
    assert_eq!(proto.code(), Some(PARSE_ERROR));
    let val = McpError::validation("invalid");
    assert_eq!(val.code(), Some(INVALID_PARAMS));

    let vm = McpError::VersionMismatch {
        requested: "a".into(),
        supported: vec!["b".into()],
    };
    assert_eq!(vm.code(), Some(MCP_PROTOCOL_VERSION_MISMATCH));
    assert!(vm.to_error_object().data.is_some());
    assert!(vm.to_string().contains("mismatch"));

    let timeout = McpError::Timeout(std::time::Duration::from_secs(1));
    assert!(timeout.is_retryable());
    assert_eq!(timeout.code(), Some(REQUEST_TIMEOUT));

    let cancelled = McpError::Cancelled("id".into());
    assert!(!cancelled.is_retryable());
    assert!(cancelled.code().is_none());

    let transport = McpError::Transport("closed".into());
    assert_eq!(transport.code(), Some(CONNECTION_CLOSED));
    assert!(!transport.is_retryable());
}

// --- tools (types) -------------------------------------------------------

#[test]
fn tool_result_builders() {
    use chuk_mcp::protocol::types::tools::{
        create_error_tool_result, create_structured_tool_result, create_text_tool_result,
        ToolInputSchema, ToolResult,
    };

    let text = create_text_tool_result("hello", false);
    assert!(text.is_valid());
    assert_eq!(text.text(), "hello");
    assert_eq!(text.is_error, Some(false));

    let mut data = serde_json::Map::new();
    data.insert("k".into(), json!("v"));
    let structured =
        create_structured_tool_result(data.clone(), Some(json!({"type": "object"})), None, false);
    assert!(structured.is_valid());
    assert_eq!(
        structured.structured_content.as_ref().unwrap()[0]
            .mime_type
            .as_deref(),
        Some("application/json")
    );

    let err = create_error_tool_result("failed", Some(data));
    assert_eq!(err.is_error, Some(true));
    assert!(err.structured_content.is_some());
    let err2 = create_error_tool_result("failed", None);
    assert!(err2.structured_content.is_none());

    let empty = ToolResult::default();
    assert!(!empty.is_valid());
    assert_eq!(empty.text(), "");

    let schema = ToolInputSchema::default();
    assert_eq!(schema.schema_type, "object");
}

// --- versioning ----------------------------------------------------------

#[test]
fn versioning_full() {
    use chuk_mcp::protocol::versioning::*;

    assert!(validate_format("2025-06-18"));
    assert!(!validate_format("2025-6-18"));
    assert!(!validate_format("nope"));
    assert!(!validate_format("2025x06x18"));

    assert!(is_supported(CURRENT_VERSION));
    assert!(!is_supported("1999-01-01"));

    assert_eq!(parse_version("2025-06-18").unwrap(), (2025, 6, 18));
    assert!(parse_version("bad").is_err());

    assert_eq!(compare("2025-06-18", "2024-11-05").unwrap(), 1);
    assert_eq!(compare("2024-11-05", "2025-06-18").unwrap(), -1);
    assert_eq!(compare(CURRENT_VERSION, CURRENT_VERSION).unwrap(), 0);
    assert!(compare("bad", "2025-06-18").is_err());
    assert!(compare("2025-06-18", "bad").is_err());

    assert!(is_newer("2025-06-18", "2024-11-05").unwrap());
    assert!(is_older("2024-11-05", "2025-06-18").unwrap());

    assert!(validate_version_compatibility(
        CURRENT_VERSION,
        CURRENT_VERSION
    ));
    assert!(!validate_version_compatibility(
        CURRENT_VERSION,
        "2024-11-05"
    ));
    assert!(!validate_version_compatibility("1999-01-01", "1999-01-01"));

    assert_eq!(
        negotiate_version(&["2025-06-18", "2024-11-05"], &["2024-11-05"]).unwrap(),
        "2024-11-05"
    );
    assert!(negotiate_version(&["2025-06-18"], &["1999-01-01"]).is_err());

    assert_eq!(MINIMUM_VERSION, "2024-11-05");
    assert_eq!(CURRENT_VERSION, "2026-07-28");
    assert_eq!(SUPPORTED_VERSIONS.len(), 4);

    // The legacy handshake list is a strict subset that excludes the stateless
    // revision — see `send_initialize_with_options`.
    assert_eq!(LEGACY_VERSIONS.len(), 3);
    assert!(LEGACY_VERSIONS.iter().all(|v| is_supported(v)));
    assert!(!LEGACY_VERSIONS.contains(&CURRENT_VERSION));

    assert!(is_modern_version(CURRENT_VERSION));
    assert!(!is_modern_version(LATEST_LEGACY_VERSION));
}

// --- batching ------------------------------------------------------------

#[test]
fn batching_full() {
    use chuk_mcp::protocol::features::batching::{
        should_reject_batch, supports_batching, BatchProcessor,
    };

    assert!(supports_batching(None));
    assert!(supports_batching(Some("")));
    assert!(supports_batching(Some("2024-11-05")));
    assert!(supports_batching(Some("2025-03-26")));
    assert!(!supports_batching(Some("2025-06-18")));
    assert!(!supports_batching(Some("2025-07-01")));
    assert!(!supports_batching(Some("2026-01-01")));
    assert!(supports_batching(Some("malformed")));
    assert!(supports_batching(Some("a-b-c")));

    assert!(should_reject_batch(Some("2025-06-18"), &json!([])));
    assert!(!should_reject_batch(Some("2025-06-18"), &json!({})));
    assert!(!should_reject_batch(Some("2024-11-05"), &json!([])));

    let mut p = BatchProcessor::default();
    assert!(p.batching_enabled);
    assert!(p.can_process_batch(&json!([])));
    p.update_protocol_version("2025-06-18");
    assert!(!p.batching_enabled);
    assert!(!p.can_process_batch(&json!([])));
    assert!(p.can_process_batch(&json!({})));
    p.update_protocol_version("2025-06-18"); // no change branch
    let err = p.create_batch_rejection_error(Some(json!(1)));
    assert_eq!(err["error"]["code"], -32600);
    assert_eq!(err["error"]["data"]["batching_supported"], false);
}
