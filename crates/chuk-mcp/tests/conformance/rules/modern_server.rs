//! How our server must answer in the `2026-07-28` era.
//!
//! This module's absence used to be the honest signal that no modern server
//! existed. It exists now, so the column does too.

use serde_json::{json, Value};

use chuk_mcp::protocol::era::ServerProfile;
use chuk_mcp::protocol::json_rpc::{create_request, JsonRpcMessage, RequestId};
use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;
use chuk_mcp::protocol::types::errors::UNSUPPORTED_PROTOCOL_VERSION;
use chuk_mcp::protocol::versioning;
use chuk_mcp::server::modern;

use crate::harness::server::{self, SERVER_NAME, TOOL_ARGUMENT, TOOL_NAME};
use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

/// A version no server will ever support, for the rejection rule.
const IMPOSSIBLE_VERSION: &str = "1999-01-01";

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::new(
            "modern.server.discover-answers",
            Era::Modern,
            Subject::Server,
            "`server/discover` returns supportedVersions, capabilities and serverInfo in `_meta`",
            discover_answers,
        ),
        Rule::new(
            "modern.server.discover-is-stateless",
            Era::Modern,
            Subject::Server,
            "`server/discover` establishes nothing and may be asked repeatedly",
            discover_is_stateless,
        ),
        Rule::new(
            "modern.server.results-carry-type",
            Era::Modern,
            Subject::Server,
            "Every modern result carries a `resultType`",
            modern_results_carry_a_type,
        ),
        Rule::new(
            "modern.server.legacy-unaffected",
            Era::Modern,
            Subject::Server,
            "A legacy request is answered without modern-only fields",
            legacy_requests_are_unaffected,
        ),
        Rule::new(
            "modern.server.rejects-unknown-version",
            Era::Modern,
            Subject::Server,
            "An unsupported declared version is rejected with -32022 and the supported list",
            rejects_an_unsupported_version,
        ),
        Rule::new(
            "modern.server.cacheable-results-carry-hints",
            Era::Modern,
            Subject::Server,
            "Every cacheable result carries `ttlMs` (>= 0) and `cacheScope` (public|private)",
            cacheable_results_carry_hints,
        ),
        Rule::new(
            "modern.server.non-cacheable-results-carry-none",
            Era::Modern,
            Subject::Server,
            "A result of an operation the specification does not list as cacheable carries no hints",
            non_cacheable_results_carry_no_hints,
        ),
        Rule::new(
            "modern.server.rejects-missing-meta",
            Era::Modern,
            Subject::Server,
            "A modern request missing `_meta` protocolVersion or clientCapabilities is refused with -32602",
            rejects_missing_meta,
        ),
        Rule::new(
            "modern.server.removed-methods-are-gone",
            Era::Modern,
            Subject::Server,
            "The RPCs this revision removed answer -32601, while a legacy request still gets them",
            removed_methods_are_gone,
        ),
        Rule::new(
            "modern.server.unsupported-version-echoes-request",
            Era::Modern,
            Subject::Server,
            "The unsupported-version error echoes the requested version alongside the supported list",
            unsupported_version_echoes_the_request,
        ),
    ]
}

/// The cacheable operations that take no parameters.
///
/// `resources/read` is the sixth, and is exercised by the suite's own
/// `caching` scenario rather than here: it needs a URI, which is a fixture
/// detail rather than a statement about caching.
const CACHEABLE: [&str; 5] = [
    MessageMethod::SERVER_DISCOVER,
    MessageMethod::TOOLS_LIST,
    MessageMethod::PROMPTS_LIST,
    MessageMethod::RESOURCES_LIST,
    MessageMethod::RESOURCES_TEMPLATES_LIST,
];

async fn cacheable_results_carry_hints() -> Verdict {
    for method in CACHEABLE {
        let result = modern_ask(method, json!({})).await?;

        let ttl = result.get("ttlMs").and_then(Value::as_i64).ok_or_else(|| {
            format!("{method}: no ttlMs, so a client has nothing to judge freshness by")
        })?;
        expect(
            ttl >= 0,
            format!("{method}: ttlMs is {ttl}, which the specification forbids"),
        )?;

        let scope = result.get("cacheScope").and_then(Value::as_str);
        expect(
            matches!(scope, Some("public") | Some("private")),
            format!("{method}: cacheScope is {scope:?}, not \"public\" or \"private\""),
        )?;
    }
    Ok(())
}

async fn non_cacheable_results_carry_no_hints() -> Verdict {
    // A tool call is an action, not a document: caching one would replay a
    // side effect.
    let result = modern_ask(
        MessageMethod::TOOLS_CALL,
        json!({"name": TOOL_NAME, "arguments": {TOOL_ARGUMENT: "world"}}),
    )
    .await?;

    expect(
        result.get("ttlMs").is_none() && result.get("cacheScope").is_none(),
        "tools/call carried caching hints, inviting a client to replay its result",
    )
}

async fn rejects_missing_meta() -> Verdict {
    use chuk_mcp::protocol::types::errors::INVALID_PARAMS;

    // Each of the three ways the block can be unusable.
    for (what, params) in [
        ("no _meta at all", json!({})),
        (
            "no protocolVersion",
            json!({"_meta": {"io.modelcontextprotocol/clientCapabilities": {}}}),
        ),
        (
            "no clientCapabilities",
            json!({"_meta": {"io.modelcontextprotocol/protocolVersion": versioning::CURRENT_VERSION}}),
        ),
    ] {
        let missing = modern::missing_required_meta(&JsonRpcMessage::Request(create_request(
            MessageMethod::TOOLS_LIST,
            Some(params),
            Some(RequestId::Num(1)),
            None,
        )));
        expect(
            missing.is_some(),
            format!("a request with {what} was not recognised as malformed"),
        )?;
    }

    // And the field that is only a SHOULD stays optional.
    let without_client_info =
        modern::missing_required_meta(&JsonRpcMessage::Request(create_request(
            MessageMethod::TOOLS_LIST,
            Some(json!({"_meta": {
                "io.modelcontextprotocol/protocolVersion": versioning::CURRENT_VERSION,
                "io.modelcontextprotocol/clientCapabilities": {},
            }})),
            Some(RequestId::Num(1)),
            None,
        )));
    expect(
        without_client_info.is_none(),
        "clientInfo is a SHOULD, but omitting it was treated as malformed",
    )?;
    let _ = INVALID_PARAMS;
    Ok(())
}

async fn removed_methods_are_gone() -> Verdict {
    use chuk_mcp::protocol::types::errors::METHOD_NOT_FOUND;

    let fixture = server::fixture();
    for method in [MessageMethod::PING, MessageMethod::LOGGING_SET_LEVEL] {
        // Modern: gone.
        let modern_request = JsonRpcMessage::Request(create_request(
            method,
            Some(modern::params_with_version(
                versioning::FIRST_MODERN_VERSION,
                json!({}),
            )),
            Some(RequestId::Str(format!("removed-{method}"))),
            None,
        ));
        let (response, _) = fixture.handle_message(modern_request, None).await;
        let error = response
            .as_ref()
            .and_then(JsonRpcMessage::error)
            .ok_or_else(|| {
                format!("{method} was answered rather than refused in the modern era")
            })?;
        expect_eq(
            &format!("{method} error code"),
            error.code,
            METHOD_NOT_FOUND,
        )?;

        // Legacy: still served, because the era is a property of the request.
        let legacy_request = JsonRpcMessage::Request(create_request(
            method,
            Some(json!({"level": "info"})),
            Some(RequestId::Str(format!("legacy-{method}"))),
            None,
        ));
        let (response, _) = fixture.handle_message(legacy_request, None).await;
        expect(
            response
                .as_ref()
                .and_then(JsonRpcMessage::error)
                .map(|e| e.code)
                != Some(METHOD_NOT_FOUND),
            format!("{method} was refused for a legacy request, which still defines it"),
        )?;
    }
    Ok(())
}

async fn unsupported_version_echoes_the_request() -> Verdict {
    let (_, _, data) = modern::unsupported_version_error(IMPOSSIBLE_VERSION);
    let data = data.ok_or("the rejection carried no data, so there is nothing to retry from")?;

    expect_eq(
        "data.requested",
        data.get("requested").and_then(Value::as_str),
        Some(IMPOSSIBLE_VERSION),
    )?;
    expect(
        data.get("supported")
            .and_then(Value::as_array)
            .is_some_and(|versions| !versions.is_empty()),
        "data.supported was absent or empty, leaving a client nothing to renegotiate with",
    )
}

/// Send a modern request — version declared in `_meta`, as every one must be.
async fn modern_ask(method: &str, params: Value) -> Result<Value, String> {
    let fixture = server::fixture();
    let request = JsonRpcMessage::Request(create_request(
        method,
        Some(modern::params_with_version(
            versioning::FIRST_MODERN_VERSION,
            params,
        )),
        Some(RequestId::Str(format!("conformance-modern-{method}"))),
        None,
    ));

    let (response, _session) = fixture.handle_message(request, None).await;
    let response = response.ok_or_else(|| format!("{method}: the server answered nothing"))?;
    if let Some(error) = response.error() {
        return Err(format!("{method}: the server returned an error: {error:?}"));
    }
    response
        .result()
        .cloned()
        .ok_or_else(|| format!("{method}: the response carried no result"))
}

async fn discover_answers() -> Verdict {
    let result = modern_ask(MessageMethod::SERVER_DISCOVER, json!({})).await?;

    // Read with the client's own parser: if that cannot make sense of it, no
    // client can, whatever our fields happen to be called.
    let profile = ServerProfile::from_discover(&result)
        .map_err(|error| format!("our own client could not read the discover result: {error}"))?;

    expect(
        profile.era.is_modern(),
        "the discover result did not classify as modern",
    )?;
    expect_eq(
        "serverInfo.name",
        profile.server_info.as_ref().map(|info| info.name.as_str()),
        Some(SERVER_NAME),
    )?;
    expect(
        !profile.supported_versions.is_empty(),
        "no supportedVersions were advertised, so a client has nothing to negotiate",
    )?;
    // Identity belongs in `_meta`, not beside capabilities as `initialize` had
    // it — the difference that has already caused one bug here.
    expect(
        result.get("serverInfo").is_none(),
        "identity was placed beside capabilities, as in the legacy initialize result",
    )
}

async fn discover_is_stateless() -> Verdict {
    // Twice, on a fresh fixture each time and with no session: a modern server
    // must not require an order of operations it cannot enforce.
    let first = modern_ask(MessageMethod::SERVER_DISCOVER, json!({})).await?;
    let second = modern_ask(MessageMethod::SERVER_DISCOVER, json!({})).await?;
    expect_eq("repeated discover results", first, second)
}

async fn modern_results_carry_a_type() -> Verdict {
    for (method, params) in [
        (MessageMethod::SERVER_DISCOVER, json!({})),
        (MessageMethod::TOOLS_LIST, json!({})),
        (
            MessageMethod::TOOLS_CALL,
            json!({"name": TOOL_NAME, "arguments": {TOOL_ARGUMENT: "world"}}),
        ),
    ] {
        let result = modern_ask(method, params).await?;
        expect_eq(
            &format!("{method} resultType"),
            result.get("resultType").and_then(Value::as_str),
            Some(RESULT_TYPE_COMPLETE),
        )?;
    }
    Ok(())
}

async fn legacy_requests_are_unaffected() -> Verdict {
    // No `_meta`, so a legacy request. The era is a property of the request,
    // and a legacy caller must not start receiving fields its revision never
    // defined.
    let fixture = server::fixture();
    let response = server::ask(
        &fixture,
        server::request("conformance-legacy", MessageMethod::TOOLS_LIST, json!({})),
    )
    .await
    .ok_or("the server answered nothing")?;

    let result = server::result_of(&response).ok_or("the response carried no result")?;
    expect(
        result.get("tools").is_some(),
        "the legacy tools/list result carried no tools",
    )?;
    expect(
        result.get("resultType").is_none(),
        format!("a legacy result carried a modern-only resultType: {result}"),
    )
}

async fn rejects_an_unsupported_version() -> Verdict {
    let fixture = server::fixture();
    let request = JsonRpcMessage::Request(create_request(
        MessageMethod::TOOLS_LIST,
        Some(modern::params_with_version(IMPOSSIBLE_VERSION, json!({}))),
        Some(RequestId::Str("conformance-bad-version".into())),
        None,
    ));

    let (response, _session) = fixture.handle_message(request, None).await;
    let response = response.ok_or("the server answered nothing")?;
    let error = response
        .error()
        .ok_or("an unsupported version was accepted")?;

    expect_eq("error code", error.code, UNSUPPORTED_PROTOCOL_VERSION)?;

    // Without the list the client has nothing to renegotiate from, which is
    // the whole point of the code.
    let supported = error
        .data
        .as_ref()
        .and_then(|data| data.get("supported"))
        .ok_or("the rejection carried no supported list to retry from")?;
    expect(
        supported
            .as_array()
            .map(|list| !list.is_empty())
            .unwrap_or(false),
        format!("the supported list was empty: {supported}"),
    )
}
