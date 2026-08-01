//! Wire fixtures and sizing constants shared by the benchmark modules.
//!
//! Everything a benchmark measures against lives here rather than inline, so
//! the numbers in one module can be compared against another's without first
//! checking whether the two used the same payload.

use serde_json::{json, Value};

use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::types::errors::UNSUPPORTED_PROTOCOL_VERSION;

/// A representative `tools/call` request, as it arrives off the wire.
pub const TOOLS_CALL_REQUEST: &str = r#"{"jsonrpc":"2.0","id":"req-42","method":"tools/call","params":{"name":"execute_sql","arguments":{"query":"select * from orders where region = 'emea' limit 10","region":"emea","timeout_ms":30000}}}"#;

/// A `tools/call` response carrying the modern result envelope: `resultType`,
/// `structuredContent`, and a server identity in `_meta`.
pub const TOOLS_CALL_RESPONSE: &str = r#"{"jsonrpc":"2.0","id":"req-42","result":{"resultType":"complete","content":[{"type":"text","text":"{\"rows\": 10}"}],"structuredContent":[{"order_id":1,"total":19.99},{"order_id":2,"total":42.00}],"isError":false,"_meta":{"serverIdentity":{"name":"orders","version":"2.1.0"}}}}"#;

/// A four-message batch — the shape a legacy client pipelines its opening
/// catalogue requests into.
pub const LIST_BATCH: &str = r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"},{"jsonrpc":"2.0","id":2,"method":"resources/list"},{"jsonrpc":"2.0","id":3,"method":"prompts/list"},{"jsonrpc":"2.0","method":"notifications/initialized"}]"#;

/// The request id used wherever a benchmark needs a fixed one.
pub const REQUEST_ID: &str = "req-42";

/// The tool every request fixture calls.
pub const TOOL_NAME: &str = "execute_sql";

/// A header value that needs no encoding — the overwhelmingly common case.
pub const PLAIN_HEADER_VALUE: &str = "eu-west-1";

/// A header value that forces the Base64 sentinel path: non-ASCII and a
/// newline, neither of which may travel raw in an HTTP header.
pub const ENCODED_HEADER_VALUE: &str = "région — naïve\nvalue";

/// How many tools the `tools/list` decoding benchmark hands back. Chosen to
/// match the catalogue size a mid-sized real server publishes; decoding cost
/// is linear in it, so the figure is only comparable across runs at one size.
pub const CATALOGUE_SIZE: usize = 32;

/// HTTP statuses the era classifier has to reach a verdict on. Named because
/// the verdict for each differs, and the difference is the point of the
/// benchmark.
pub mod status {
    /// Success: only a modern server answers a modern request this way.
    pub const OK: u16 = 200;
    /// Ambiguous: the body decides. A modern protocol error means modern.
    pub const BAD_REQUEST: u16 = 400;
    /// A bare 404 means the endpoint never understood the request: legacy.
    pub const NOT_FOUND: u16 = 404;
}

/// The body a modern server returns when it rejects the declared version.
pub fn unsupported_version_error_body() -> String {
    json!({
        "jsonrpc": "2.0",
        "id": REQUEST_ID,
        "error": {
            "code": UNSUPPORTED_PROTOCOL_VERSION,
            "message": "unsupported protocol version",
            "data": {"supported": [chuk_mcp::protocol::versioning::FIRST_MODERN_VERSION]},
        },
    })
    .to_string()
}

/// The body a legacy server returns for an unknown path — HTML, not JSON-RPC.
pub const NOT_FOUND_BODY: &str = "Not Found";

/// `tools/call` params for a query that also carries promotable arguments.
pub fn tools_call_params() -> Value {
    json!({
        "name": TOOL_NAME,
        "arguments": tools_call_arguments(),
    })
}

/// The arguments half of [`tools_call_params`], for header promotion.
pub fn tools_call_arguments() -> Value {
    json!({
        "query": "select 1",
        "region": "emea",
        "tenant": "acme",
    })
}

/// A tool schema declaring two `x-mcp-header` promotions, per the spec's
/// worked example.
pub fn tools_call_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string"},
            "region": {"type": "string", "x-mcp-header": "Region"},
            "tenant": {"type": "string", "x-mcp-header": "Tenant"},
        },
        "required": ["query"],
    })
}

/// A `tools/list` result carrying [`CATALOGUE_SIZE`] tools.
pub fn tools_list_result() -> Value {
    let tools: Vec<Value> = (0..CATALOGUE_SIZE)
        .map(|index| {
            json!({
                "name": format!("{TOOL_NAME}_{index}"),
                "description": "A tool that does a thing",
                "inputSchema": tools_call_schema(),
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// The `result` member of [`TOOLS_CALL_RESPONSE`], ready to decode.
pub fn tools_call_result_value() -> Value {
    let response: Value = serde_json::from_str(TOOLS_CALL_RESPONSE)
        .expect("TOOLS_CALL_RESPONSE fixture must be valid JSON");
    response
        .get("result")
        .expect("TOOLS_CALL_RESPONSE fixture must carry a result")
        .clone()
}

/// The method every request fixture uses, re-exported so benchmark modules
/// never spell it as a literal.
pub const CALL_METHOD: &str = MessageMethod::TOOLS_CALL;

// --- Server-side fixtures --------------------------------------------------

/// What a tool that fails reports.
pub const TOOL_FAILURE: &str = "the upstream database refused the connection";

/// A resource template with one variable, the shape the specification's
/// resource templates use.
pub const RESOURCE_TEMPLATE: &str = "db://orders/{id}/detail";

/// A URI the template binds.
pub const RESOURCE_URI_MATCHING: &str = "db://orders/48219/detail";

/// A URI of the same length and shape that the template does not bind, so the
/// miss is measured against a realistic near-match rather than a short string
/// that fails on the first byte.
pub const RESOURCE_URI_UNMATCHING: &str = "db://orders/48219/summary";

/// The three shapes a tool handler's return value comes in. Each takes a
/// different arm of the result formatter, and they do not cost the same.
pub fn tool_handler_text() -> Value {
    json!("Ten orders matched, totalling 419.94 across three regions.")
}

/// A handler that returns a content block it arranged itself.
pub fn tool_handler_content_block() -> Value {
    json!({"type": "image", "data": "iVBORw0KGgoAAAANSUhEUg==", "mimeType": "image/png"})
}

/// A handler that returns data to be rendered — the arm that has to serialize.
pub fn tool_handler_data() -> Value {
    json!({
        "rows": 10,
        "region": "emea",
        "total": 419.94,
        "orders": [{"order_id": 1, "total": 19.99}, {"order_id": 2, "total": 42.00}],
    })
}

/// The headers of an ordinary request to a local server.
pub fn loopback_headers() -> hyper::header::HeaderMap {
    headers(&[
        ("host", "127.0.0.1:3000"),
        ("origin", "http://localhost:3000"),
    ])
}

/// The headers of a DNS-rebinding attempt: the browser is talking to loopback
/// and says so, but the page asking is not one this server serves.
pub fn rebinding_headers() -> hyper::header::HeaderMap {
    headers(&[
        ("host", "127.0.0.1:3000"),
        ("origin", "http://evil.example"),
    ])
}

fn headers(pairs: &[(&str, &str)]) -> hyper::header::HeaderMap {
    let mut headers = hyper::header::HeaderMap::new();
    for (name, value) in pairs {
        headers.insert(
            hyper::header::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
            value.parse().expect("a header value"),
        );
    }
    headers
}

/// A progress notification, the message most often framed as an event.
pub fn progress_notification() -> Value {
    json!({
        "jsonrpc": "2.0",
        "method": MessageMethod::NOTIFICATION_PROGRESS,
        "params": {"progressToken": REQUEST_ID, "progress": 50.0, "total": 100.0},
    })
}

/// The params of a `completion/complete` request.
pub fn completion_params() -> serde_json::Map<String, Value> {
    json!({
        "ref": {"type": "ref/prompt", "name": "summarise"},
        "argument": {"name": "region", "value": "em"},
    })
    .as_object()
    .expect("the completion fixture is an object")
    .clone()
}
