//! What the `2026-07-28` revision requires of every outbound request.
//!
//! The modern era has no handshake, so there is no transcript to inspect:
//! conformance is a property of each request envelope on its own. These rules
//! therefore assert on what `build_envelope` produces, which is exactly what
//! the modern transports put on the wire.

use serde_json::{json, Map, Value};

use chuk_mcp::protocol::envelope::{
    build_envelope, Envelope, BASE64_SENTINEL_PREFIX, HEADER_METHOD, HEADER_NAME,
    HEADER_PARAM_PREFIX, HEADER_PROTOCOL_VERSION,
};
use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::meta::{self, CLIENT_CAPABILITIES, PROTOCOL_VERSION};
use chuk_mcp::protocol::versioning;
use chuk_mcp::ClientIdentity;

use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

/// The tool the envelope rules build requests for.
const TOOL_NAME: &str = "execute_sql";
/// A resource uri, for the `Mcp-Name` rule's second shape.
const RESOURCE_URI: &str = "db://orders";

/// The header a tool parameter is promoted to in the spec's worked example.
const PROMOTED_HEADER: &str = "Region";
const PROMOTED_PARAMETER: &str = "region";
const PROMOTED_VALUE: &str = "emea";
/// A value that cannot travel raw in an HTTP header, forcing the sentinel.
const UNSAFE_VALUE: &str = "eu\r\nX-Injected: yes";

/// The header that must never appear on a modern request: sessions do not
/// exist in this revision, and sending one can make a dual-era server select
/// legacy semantics.
const SESSION_HEADER: &str = "Mcp-Session-Id";

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::sync(
            "modern.client.meta-protocol-version",
            Era::Modern,
            Subject::Client,
            "Every request carries `_meta.protocolVersion`",
            meta_declares_protocol_version,
        ),
        Rule::sync(
            "modern.client.meta-capabilities",
            Era::Modern,
            Subject::Client,
            "Every request carries `_meta.clientCapabilities`",
            meta_declares_capabilities,
        ),
        Rule::sync(
            "modern.client.header-mirrors-version",
            Era::Modern,
            Subject::Client,
            "`MCP-Protocol-Version` equals `_meta.protocolVersion`",
            version_header_mirrors_meta,
        ),
        Rule::sync(
            "modern.client.header-mirrors-method",
            Era::Modern,
            Subject::Client,
            "`Mcp-Method` equals the JSON-RPC method",
            method_header_mirrors_body,
        ),
        Rule::sync(
            "modern.client.header-mirrors-name",
            Era::Modern,
            Subject::Client,
            "`Mcp-Name` equals params.name, or params.uri when there is no name",
            name_header_mirrors_body,
        ),
        Rule::sync(
            "modern.client.param-promotion",
            Era::Modern,
            Subject::Client,
            "Parameters marked `x-mcp-header` are promoted to `Mcp-Param-*` headers",
            promotes_declared_parameters,
        ),
        Rule::sync(
            "modern.client.header-value-encoding",
            Era::Modern,
            Subject::Client,
            "A header value that is not plain-safe is Base64 sentinel-encoded",
            encodes_unsafe_header_values,
        ),
        Rule::sync(
            "modern.client.no-session-header",
            Era::Modern,
            Subject::Client,
            "No `Mcp-Session-Id` is sent: this revision has no sessions",
            sends_no_session_header,
        ),
        Rule::sync(
            "modern.client.discover-not-initialize",
            Era::Modern,
            Subject::Client,
            "Capability discovery uses `server/discover`; `initialize` is gone",
            discovery_uses_server_discover,
        ),
    ]
}

/// A `tools/call` envelope, as the modern transports build one.
fn tools_call_envelope() -> Result<Envelope, String> {
    envelope_for(
        MessageMethod::TOOLS_CALL,
        json!({
            "name": TOOL_NAME,
            "arguments": {PROMOTED_PARAMETER: PROMOTED_VALUE},
        }),
    )
}

fn envelope_for(method: &str, params: Value) -> Result<Envelope, String> {
    build_envelope(
        method,
        Some(params),
        versioning::FIRST_MODERN_VERSION,
        &ClientIdentity::chuk(),
    )
    .map_err(|error| format!("building the {method} envelope failed: {error}"))
}

/// The `_meta` block of an envelope's params. Its keys are namespaced, so
/// every lookup goes through the protocol's own constants.
fn meta_of(envelope: &Envelope) -> Result<&Map<String, Value>, String> {
    meta::meta_of(&envelope.params)
        .ok_or_else(|| format!("the envelope carried no `_meta`: {}", envelope.params))
}

fn meta_declares_protocol_version() -> Verdict {
    let envelope = tools_call_envelope()?;
    let meta = meta_of(&envelope)?;
    expect_eq(
        PROTOCOL_VERSION,
        meta.get(PROTOCOL_VERSION).and_then(Value::as_str),
        Some(versioning::FIRST_MODERN_VERSION),
    )
}

fn meta_declares_capabilities() -> Verdict {
    let envelope = tools_call_envelope()?;
    let meta = meta_of(&envelope)?;
    expect(
        meta.get(CLIENT_CAPABILITIES).is_some(),
        format!("`_meta` omitted {CLIENT_CAPABILITIES}"),
    )
}

fn version_header_mirrors_meta() -> Verdict {
    let envelope = tools_call_envelope()?;
    let meta = meta_of(&envelope)?;
    expect_eq(
        HEADER_PROTOCOL_VERSION,
        envelope.header(HEADER_PROTOCOL_VERSION),
        meta.get(PROTOCOL_VERSION).and_then(Value::as_str),
    )
}

fn method_header_mirrors_body() -> Verdict {
    let envelope = tools_call_envelope()?;
    expect_eq(
        HEADER_METHOD,
        envelope.header(HEADER_METHOD),
        Some(MessageMethod::TOOLS_CALL),
    )
}

fn name_header_mirrors_body() -> Verdict {
    let by_name = tools_call_envelope()?;
    expect_eq(
        "Mcp-Name from params.name",
        by_name.header(HEADER_NAME),
        Some(TOOL_NAME),
    )?;

    // A request with a uri and no name mirrors the uri instead.
    let by_uri = envelope_for(MessageMethod::RESOURCES_READ, json!({"uri": RESOURCE_URI}))?;
    expect_eq(
        "Mcp-Name from params.uri",
        by_uri.header(HEADER_NAME),
        Some(RESOURCE_URI),
    )
}

/// The schema and arguments the promotion rules share.
fn promotion_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            PROMOTED_PARAMETER: {"type": "string", "x-mcp-header": PROMOTED_HEADER},
        },
    })
}

fn promoted_header_name() -> String {
    format!("{HEADER_PARAM_PREFIX}{PROMOTED_HEADER}")
}

fn promotes_declared_parameters() -> Verdict {
    let mut envelope = tools_call_envelope()?;
    envelope
        .promote_tool_params(
            &promotion_schema(),
            &json!({PROMOTED_PARAMETER: PROMOTED_VALUE}),
        )
        .map_err(|error| format!("promotion failed: {error}"))?;

    expect_eq(
        &promoted_header_name(),
        envelope.header(&promoted_header_name()),
        Some(PROMOTED_VALUE),
    )
}

fn encodes_unsafe_header_values() -> Verdict {
    let mut envelope = tools_call_envelope()?;
    envelope
        .promote_tool_params(
            &promotion_schema(),
            &json!({PROMOTED_PARAMETER: UNSAFE_VALUE}),
        )
        .map_err(|error| format!("promotion failed: {error}"))?;

    let value = envelope
        .header(&promoted_header_name())
        .ok_or_else(|| format!("{} was not promoted at all", promoted_header_name()))?;

    expect(
        value.starts_with(BASE64_SENTINEL_PREFIX),
        format!("an unsafe value was sent unencoded: {value:?}"),
    )?;
    expect(
        !value.contains('\r') && !value.contains('\n'),
        format!("the encoded value still contained a line break: {value:?}"),
    )
}

fn sends_no_session_header() -> Verdict {
    let envelope = tools_call_envelope()?;
    expect(
        envelope.header(SESSION_HEADER).is_none(),
        format!("a modern request carried {SESSION_HEADER}"),
    )
}

fn discovery_uses_server_discover() -> Verdict {
    let envelope = envelope_for(MessageMethod::SERVER_DISCOVER, json!({}))?;
    expect_eq(
        "discovery method",
        envelope.header(HEADER_METHOD),
        Some(MessageMethod::SERVER_DISCOVER),
    )?;
    // The envelope must be self-describing: a stateless request cannot rely on
    // a prior handshake to have declared the version.
    let meta = meta_of(&envelope)?;
    expect(
        meta.get(PROTOCOL_VERSION).is_some(),
        "server/discover did not declare a protocol version",
    )
}
