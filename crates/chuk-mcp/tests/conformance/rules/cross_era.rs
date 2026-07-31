//! Rules both eras must satisfy.
//!
//! These are the guarantees that let one caller read results from a legacy
//! peer and a modern peer without branching — the compatibility promise the
//! dual-era design rests on. If they break, the era stops being an
//! implementation detail and becomes the caller's problem.

use serde_json::{json, Value};

use chuk_mcp::protocol::json_rpc::{parse_message_str, JsonRpcMessage};
use chuk_mcp::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;
use chuk_mcp::protocol::messages::tools::ToolResult;
use chuk_mcp::protocol::versioning;

use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

/// A `resultType` that is not "complete", for the preservation rule.
const INCOMPLETE_RESULT_TYPE: &str = "incomplete";

/// A batch, which only some legacy versions permit.
const BATCH: &str =
    r#"[{"jsonrpc":"2.0","id":1,"method":"tools/list"},{"jsonrpc":"2.0","id":2,"method":"ping"}]"#;
/// How many messages [`BATCH`] carries.
const BATCH_ENTRY_COUNT: usize = 2;

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::sync(
            "both.protocol.result-type-default",
            Era::Both,
            Subject::Protocol,
            "A result with no `resultType` reads as `complete`",
            absent_result_type_defaults_to_complete,
        ),
        Rule::sync(
            "both.protocol.result-type-preserved",
            Era::Both,
            Subject::Protocol,
            "A declared `resultType` is preserved verbatim",
            declared_result_type_is_preserved,
        ),
        Rule::sync(
            "both.protocol.error-flag-respected",
            Era::Both,
            Subject::Protocol,
            "`isError` marks a tool result as failed without a JSON-RPC error",
            is_error_is_respected,
        ),
        Rule::sync(
            "both.protocol.batch-parses",
            Era::Both,
            Subject::Protocol,
            "A JSON-RPC batch parses as a batch, not as a single message",
            batches_parse_as_batches,
        ),
        Rule::sync(
            "both.protocol.version-classification",
            Era::Both,
            Subject::Protocol,
            "Every supported version classifies into exactly one era",
            every_supported_version_has_an_era,
        ),
        Rule::sync(
            "both.protocol.unsupported-version-rejected",
            Era::Both,
            Subject::Protocol,
            "Negotiation fails when no version is shared",
            negotiation_fails_without_overlap,
        ),
    ]
}

/// Decode a tool result, reporting the failure rather than panicking.
fn decode(result: Value) -> Result<ToolResult, String> {
    serde_json::from_value(result).map_err(|error| format!("the result did not decode: {error}"))
}

fn absent_result_type_defaults_to_complete() -> Verdict {
    // The legacy shape: no resultType at all.
    let decoded = decode(json!({"content": [{"type": "text", "text": "ok"}]}))?;
    expect_eq(
        "resultType of a legacy result",
        decoded.result_type.as_str(),
        RESULT_TYPE_COMPLETE,
    )
}

fn declared_result_type_is_preserved() -> Verdict {
    let decoded = decode(json!({
        "content": [],
        "resultType": INCOMPLETE_RESULT_TYPE,
    }))?;
    expect_eq(
        "declared resultType",
        decoded.result_type.as_str(),
        INCOMPLETE_RESULT_TYPE,
    )
}

fn is_error_is_respected() -> Verdict {
    let failed = decode(json!({
        "content": [{"type": "text", "text": "boom"}],
        "isError": true,
    }))?;
    expect(failed.is_error, "isError: true did not survive decoding")?;

    let succeeded = decode(json!({"content": []}))?;
    expect(
        !succeeded.is_error,
        "a result with no isError decoded as failed",
    )
}

fn batches_parse_as_batches() -> Verdict {
    let parsed =
        parse_message_str(BATCH).map_err(|error| format!("the batch did not parse: {error}"))?;
    let entries = match &parsed {
        JsonRpcMessage::BatchRequest(entries) | JsonRpcMessage::BatchResponse(entries) => entries,
        other => {
            return Err(format!(
                "a JSON-RPC batch parsed as {:?}, not as a batch",
                std::mem::discriminant(other)
            ))
        }
    };
    expect(
        entries.len() == BATCH_ENTRY_COUNT,
        format!(
            "the batch parsed with {} entries, expected {BATCH_ENTRY_COUNT}",
            entries.len()
        ),
    )?;
    expect(parsed.is_batch(), "is_batch() disagreed with the parse")
}

fn every_supported_version_has_an_era() -> Verdict {
    for version in versioning::SUPPORTED_VERSIONS {
        let modern = versioning::is_modern_version(version);
        let legacy = versioning::LEGACY_VERSIONS.contains(version);
        expect(
            modern != legacy,
            format!(
                "{version} classifies as modern={modern} and legacy={legacy}; \
                 it must be exactly one"
            ),
        )?;
    }
    Ok(())
}

fn negotiation_fails_without_overlap() -> Verdict {
    let unshared = ["1999-01-01"];
    let outcome = versioning::negotiate_version(versioning::SUPPORTED_VERSIONS, &unshared);
    expect(
        outcome.is_err(),
        format!("negotiation succeeded against an unsupported peer: {outcome:?}"),
    )
}
