//! Coverage-oriented tests for the JSON-RPC message layer.

use serde_json::json;

use chuk_mcp::protocol::json_rpc::{
    create_error_response, create_notification, create_request, create_response, parse_message,
    parse_message_str, JsonRpcMessage, RequestId,
};

#[test]
fn request_id_display_and_from() {
    assert_eq!(RequestId::Num(7).to_string(), "7");
    assert_eq!(RequestId::Str("a".into()).to_string(), "a");
    assert_eq!(RequestId::from("s"), RequestId::Str("s".into()));
    assert_eq!(RequestId::from("s".to_string()), RequestId::Str("s".into()));
    assert_eq!(RequestId::from(9i64), RequestId::Num(9));
}

#[test]
fn message_accessors_all_variants() {
    let req = JsonRpcMessage::Request(create_request(
        "m",
        Some(json!({"p": 1})),
        Some(1.into()),
        None,
    ));
    assert!(req.is_request());
    assert_eq!(req.method(), Some("m"));
    assert!(req.params().is_some());
    assert!(req.result().is_none());
    assert!(req.error().is_none());

    let notif = JsonRpcMessage::Notification(create_notification("n", Some(json!({}))));
    assert!(notif.is_notification());
    assert!(notif.id().is_none());
    assert_eq!(notif.method(), Some("n"));
    assert!(notif.params().is_some());
    assert!(notif.result().is_none());

    let resp = JsonRpcMessage::Response(create_response(
        RequestId::Num(2),
        Some(json!({"ok": true})),
    ));
    assert!(resp.is_response());
    assert!(resp.result().is_some());
    assert!(resp.method().is_none());
    assert!(resp.params().is_none());
    assert!(resp.error().is_none());

    let err = JsonRpcMessage::Error(create_error_response(
        RequestId::Num(3),
        -1,
        "e",
        Some(json!("d")),
    ));
    assert!(err.is_error_response());
    assert!(err.error().is_some());
    assert_eq!(err.error().unwrap().data, Some(json!("d")));
    assert!(err.result().is_none());

    // empty result defaults to {}
    let empty = create_response(RequestId::Num(4), None);
    assert_eq!(empty.result, json!({}));
}

#[test]
fn batches_and_serialization() {
    let batch_req = parse_message(&json!([
        {"jsonrpc": "2.0", "id": 1, "method": "a"},
        {"jsonrpc": "2.0", "method": "b"}
    ]))
    .unwrap();
    assert!(batch_req.is_batch());
    assert!(matches!(batch_req, JsonRpcMessage::BatchRequest(_)));
    // batch has no id/method/params/result/error
    assert!(batch_req.id().is_none());
    assert!(batch_req.method().is_none());
    assert!(batch_req.params().is_none());
    assert!(batch_req.result().is_none());
    assert!(batch_req.error().is_none());
    // to_value / to_json round-trips a batch
    let json = batch_req.to_json();
    assert_eq!(parse_message_str(&json).unwrap(), batch_req);

    let batch_resp = parse_message(&json!([
        {"jsonrpc": "2.0", "id": 1, "result": {}},
        {"jsonrpc": "2.0", "id": 2, "error": {"code": -1, "message": "x"}}
    ]))
    .unwrap();
    assert!(matches!(batch_resp, JsonRpcMessage::BatchResponse(_)));

    // serde Serialize/Deserialize via the enum
    let value = serde_json::to_value(&batch_resp).unwrap();
    let back: JsonRpcMessage = serde_json::from_value(value).unwrap();
    assert_eq!(back, batch_resp);
}

#[test]
fn parse_errors() {
    // mixed batch
    assert!(parse_message(&json!([
        {"jsonrpc": "2.0", "id": 1, "method": "a"},
        {"jsonrpc": "2.0", "id": 1, "result": {}}
    ]))
    .is_err());
    // non-object, non-array
    assert!(parse_message(&json!(5)).is_err());
    // bad version
    assert!(parse_message(&json!({"jsonrpc": "1.0", "id": 1, "method": "a"})).is_err());
    // invalid structure: id only, no method/result/error
    assert!(parse_message(&json!({"jsonrpc": "2.0", "id": 1})).is_err());
    // both result and error -> not a clean response/error
    assert!(parse_message(
        &json!({"jsonrpc": "2.0", "id": 1, "result": {}, "error": {"code": 1, "message": "x"}})
    )
    .is_err());
    // bad json string
    assert!(parse_message_str("{not json").is_err());
}
