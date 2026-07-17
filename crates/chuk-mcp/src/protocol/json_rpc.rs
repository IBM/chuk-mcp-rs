//! JSON-RPC 2.0 message types, mirroring `chuk_mcp.protocol.messages.json_rpc_message`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::protocol::types::errors::{McpError, PARSE_ERROR};

/// A JSON-RPC request/response id: string or integer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Num(i64),
    Str(String),
}

impl std::fmt::Display for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestId::Num(n) => write!(f, "{n}"),
            RequestId::Str(s) => write!(f, "{s}"),
        }
    }
}

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        RequestId::Str(s.to_string())
    }
}

impl From<String> for RequestId {
    fn from(s: String) -> Self {
        RequestId::Str(s)
    }
}

impl From<i64> for RequestId {
    fn from(n: i64) -> Self {
        RequestId::Num(n)
    }
}

/// A progress token: string or integer (same wire shape as [`RequestId`]).
pub type ProgressToken = RequestId;

/// The JSON-RPC error object: `{code, message, data?}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorObject {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A request that expects a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: RequestId,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A notification which does not expect a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// A successful (non-error) response to a request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: RequestId,
    pub result: Value,
}

/// A response to a request that indicates an error occurred.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub jsonrpc: String,
    pub id: RequestId,
    pub error: ErrorObject,
}

/// Any valid JSON-RPC message, including batches.
///
/// This is the unified type carried over transport streams; it plays the role
/// of both `JSONRPCMessage` and the specific message types in the Python
/// package.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonRpcMessage {
    Request(JsonRpcRequest),
    Notification(JsonRpcNotification),
    Response(JsonRpcResponse),
    Error(JsonRpcError),
    /// A batch of requests and/or notifications.
    BatchRequest(Vec<JsonRpcMessage>),
    /// A batch of responses and/or errors.
    BatchResponse(Vec<JsonRpcMessage>),
}

impl JsonRpcMessage {
    /// The message id, if it has one.
    pub fn id(&self) -> Option<&RequestId> {
        match self {
            JsonRpcMessage::Request(r) => Some(&r.id),
            JsonRpcMessage::Response(r) => Some(&r.id),
            JsonRpcMessage::Error(e) => Some(&e.id),
            _ => None,
        }
    }

    /// The method name, if this is a request or notification.
    pub fn method(&self) -> Option<&str> {
        match self {
            JsonRpcMessage::Request(r) => Some(&r.method),
            JsonRpcMessage::Notification(n) => Some(&n.method),
            _ => None,
        }
    }

    /// The params, if this is a request or notification.
    pub fn params(&self) -> Option<&Value> {
        match self {
            JsonRpcMessage::Request(r) => r.params.as_ref(),
            JsonRpcMessage::Notification(n) => n.params.as_ref(),
            _ => None,
        }
    }

    /// The result, if this is a successful response.
    pub fn result(&self) -> Option<&Value> {
        match self {
            JsonRpcMessage::Response(r) => Some(&r.result),
            _ => None,
        }
    }

    /// The error object, if this is an error response.
    pub fn error(&self) -> Option<&ErrorObject> {
        match self {
            JsonRpcMessage::Error(e) => Some(&e.error),
            _ => None,
        }
    }

    pub fn is_request(&self) -> bool {
        matches!(self, JsonRpcMessage::Request(_))
    }

    pub fn is_notification(&self) -> bool {
        matches!(self, JsonRpcMessage::Notification(_))
    }

    pub fn is_response(&self) -> bool {
        matches!(self, JsonRpcMessage::Response(_))
    }

    pub fn is_error_response(&self) -> bool {
        matches!(self, JsonRpcMessage::Error(_))
    }

    pub fn is_batch(&self) -> bool {
        matches!(
            self,
            JsonRpcMessage::BatchRequest(_) | JsonRpcMessage::BatchResponse(_)
        )
    }

    /// Serialize to a JSON [`Value`].
    pub fn to_value(&self) -> Value {
        match self {
            JsonRpcMessage::Request(r) => serde_json::to_value(r).expect("serialize request"),
            JsonRpcMessage::Notification(n) => {
                serde_json::to_value(n).expect("serialize notification")
            }
            JsonRpcMessage::Response(r) => serde_json::to_value(r).expect("serialize response"),
            JsonRpcMessage::Error(e) => serde_json::to_value(e).expect("serialize error"),
            JsonRpcMessage::BatchRequest(msgs) | JsonRpcMessage::BatchResponse(msgs) => {
                Value::Array(msgs.iter().map(|m| m.to_value()).collect())
            }
        }
    }

    /// Serialize to a JSON string.
    pub fn to_json(&self) -> String {
        self.to_value().to_string()
    }
}

impl Serialize for JsonRpcMessage {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonRpcMessage {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        parse_message(&value).map_err(serde::de::Error::custom)
    }
}

/// Create a request message with an optional progress token.
///
/// If `id` is `None` a UUID v4 string id is generated. If `progress_token` is
/// provided it is stored under `params._meta.progressToken`.
pub fn create_request(
    method: &str,
    params: Option<Value>,
    id: Option<RequestId>,
    progress_token: Option<ProgressToken>,
) -> JsonRpcRequest {
    let id = id.unwrap_or_else(|| RequestId::Str(uuid::Uuid::new_v4().to_string()));

    let params = match progress_token {
        None => params,
        Some(token) => {
            let mut map = match params {
                Some(Value::Object(map)) => map,
                _ => Map::new(),
            };
            let meta = map
                .entry("_meta")
                .or_insert_with(|| Value::Object(Map::new()));
            if let Value::Object(meta_map) = meta {
                meta_map.insert(
                    "progressToken".to_string(),
                    serde_json::to_value(token).expect("serialize progress token"),
                );
            }
            Some(Value::Object(map))
        }
    };

    JsonRpcRequest {
        jsonrpc: "2.0".to_string(),
        id,
        method: method.to_string(),
        params,
    }
}

/// Create a notification message.
pub fn create_notification(method: &str, params: Option<Value>) -> JsonRpcNotification {
    JsonRpcNotification {
        jsonrpc: "2.0".to_string(),
        method: method.to_string(),
        params,
    }
}

/// Create a successful response message. A `None` result becomes `{}` per spec.
pub fn create_response(id: RequestId, result: Option<Value>) -> JsonRpcResponse {
    JsonRpcResponse {
        jsonrpc: "2.0".to_string(),
        id,
        result: result.unwrap_or_else(|| Value::Object(Map::new())),
    }
}

/// Create an error response message.
pub fn create_error_response(
    id: RequestId,
    code: i64,
    message: &str,
    data: Option<Value>,
) -> JsonRpcError {
    JsonRpcError {
        jsonrpc: "2.0".to_string(),
        id,
        error: ErrorObject {
            code,
            message: message.to_string(),
            data,
        },
    }
}

/// Parse incoming JSON data into the appropriate JSON-RPC message type.
///
/// Batches are parsed recursively; a batch mixing requests and responses is
/// rejected, matching the Python implementation.
pub fn parse_message(data: &Value) -> Result<JsonRpcMessage, McpError> {
    if let Value::Array(items) = data {
        let messages: Vec<JsonRpcMessage> = items
            .iter()
            .map(parse_message)
            .collect::<Result<_, _>>()?;

        let all_requests = messages
            .iter()
            .all(|m| m.is_request() || m.is_notification());
        let all_responses = messages
            .iter()
            .all(|m| m.is_response() || m.is_error_response());

        return if all_requests {
            Ok(JsonRpcMessage::BatchRequest(messages))
        } else if all_responses {
            Ok(JsonRpcMessage::BatchResponse(messages))
        } else {
            Err(McpError::protocol(
                PARSE_ERROR,
                "Batch contains mixed request/response types",
            ))
        };
    }

    let obj = data.as_object().ok_or_else(|| {
        McpError::protocol(PARSE_ERROR, "Message must be an object or array")
    })?;

    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(McpError::protocol(
            PARSE_ERROR,
            "Missing or invalid jsonrpc version",
        ));
    }

    let has_id = obj.contains_key("id");
    let has_method = obj.contains_key("method");
    let has_result = obj.contains_key("result");
    let has_error = obj.contains_key("error");

    let parse_err =
        |e: serde_json::Error| McpError::protocol(PARSE_ERROR, format!("Invalid message: {e}"));

    if has_method && has_id {
        Ok(JsonRpcMessage::Request(
            serde_json::from_value(data.clone()).map_err(parse_err)?,
        ))
    } else if has_method {
        Ok(JsonRpcMessage::Notification(
            serde_json::from_value(data.clone()).map_err(parse_err)?,
        ))
    } else if has_id && has_result && !has_error {
        Ok(JsonRpcMessage::Response(
            serde_json::from_value(data.clone()).map_err(parse_err)?,
        ))
    } else if has_id && has_error && !has_result {
        Ok(JsonRpcMessage::Error(
            serde_json::from_value(data.clone()).map_err(parse_err)?,
        ))
    } else {
        Err(McpError::protocol(
            PARSE_ERROR,
            "Invalid JSON-RPC message structure",
        ))
    }
}

/// Parse a JSON string into a JSON-RPC message.
pub fn parse_message_str(data: &str) -> Result<JsonRpcMessage, McpError> {
    let value: Value = serde_json::from_str(data)
        .map_err(|e| McpError::protocol(PARSE_ERROR, format!("JSON decode error: {e}")))?;
    parse_message(&value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_request() {
        let msg = parse_message(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).unwrap();
        assert!(msg.is_request());
        assert_eq!(msg.method(), Some("ping"));
        assert_eq!(msg.id(), Some(&RequestId::Num(1)));
    }

    #[test]
    fn parse_notification() {
        let msg = parse_message(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}),
        )
        .unwrap();
        assert!(msg.is_notification());
    }

    #[test]
    fn parse_response_and_error() {
        let msg =
            parse_message(&json!({"jsonrpc": "2.0", "id": "a", "result": {"ok": true}})).unwrap();
        assert!(msg.is_response());

        let msg = parse_message(
            &json!({"jsonrpc": "2.0", "id": "a", "error": {"code": -32601, "message": "nope"}}),
        )
        .unwrap();
        assert!(msg.is_error_response());
        assert_eq!(msg.error().unwrap().code, -32601);
    }

    #[test]
    fn rejects_mixed_batch() {
        let result = parse_message(&json!([
            {"jsonrpc": "2.0", "id": 1, "method": "ping"},
            {"jsonrpc": "2.0", "id": 1, "result": {}}
        ]));
        assert!(result.is_err());
    }

    #[test]
    fn rejects_bad_version() {
        assert!(parse_message(&json!({"jsonrpc": "1.0", "id": 1, "method": "ping"})).is_err());
    }

    #[test]
    fn progress_token_lands_in_meta() {
        let req = create_request("tools/call", Some(json!({"name": "t"})), None, Some("tok".into()));
        assert_eq!(req.params.unwrap()["_meta"]["progressToken"], json!("tok"));
    }

    #[test]
    fn roundtrip_serialization() {
        let req = create_request("ping", None, Some(RequestId::Num(7)), None);
        let msg = JsonRpcMessage::Request(req);
        let parsed = parse_message_str(&msg.to_json()).unwrap();
        assert_eq!(parsed, msg);
    }
}
