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
    ]
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
