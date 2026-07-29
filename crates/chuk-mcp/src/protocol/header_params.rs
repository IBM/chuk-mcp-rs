//! `x-mcp-header` — mirroring tool parameters into HTTP headers.
//!
//! A server may annotate a tool parameter with `x-mcp-header` so its value is
//! also sent as `Mcp-Param-{Name}`, letting a gateway route or rate-limit on
//! (say) a tenant or region without parsing the body. Servers **MAY** use it;
//! clients **MUST** support it.
//!
//! The annotation is heavily constrained, and the constraints are all about not
//! letting a tool definition become an injection vector or an ambiguity:
//!
//! * The name must be a valid HTTP field-name token — no control characters, no
//!   CR/LF, so a name cannot inject a header break.
//! * Names must be case-insensitively unique, because HTTP field names are
//!   case-insensitive and two spellings would collide.
//! * Only `string`, `integer` and `boolean` may be promoted. `number` is
//!   excluded: its text form is not canonical, so client and server could
//!   disagree on `1.0` versus `1` and trip header/body validation.
//! * The property must be *statically reachable* — a chain of `properties` keys
//!   only. Not through `items`, `oneOf`/`anyOf`/`allOf`/`not`, `if`/`then`/`else`
//!   or `$ref`, because in those positions the value's location depends on the
//!   instance and no fixed header can describe it.
//!
//! A violation invalidates the whole tool definition. Per the spec a client
//! **MUST** exclude that tool from `tools/list` rather than failing the entire
//! list — one malformed definition must not deny the user every other tool. So
//! [`collect`] returns an error for the offending tool and the caller drops just
//! that one; see [`crate::protocol::envelope`] for where the headers get
//! attached.

use serde_json::{Map, Value};

use crate::protocol::types::errors::McpError;

/// The annotation key a server uses to request promotion.
pub const ANNOTATION: &str = "x-mcp-header";

/// Largest integer that survives a JavaScript round trip: `2^53 - 1`.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;
/// Smallest such integer: `-(2^53 - 1)`.
pub const MIN_SAFE_INTEGER: i64 = -MAX_SAFE_INTEGER;

/// A parameter the tool definition asked to be mirrored into a header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderParam {
    /// The `{Name}` in `Mcp-Param-{Name}`, exactly as the server wrote it.
    pub name: String,
    /// Property path from the schema root, every step a `properties` key.
    pub path: Vec<String>,
}

/// Collect and validate every `x-mcp-header` declaration in an `inputSchema`.
///
/// Returns an error if the schema violates any constraint — the caller should
/// then exclude this one tool, not fail the whole listing.
pub fn collect(input_schema: &Value) -> Result<Vec<HeaderParam>, McpError> {
    let mut found = Vec::new();
    walk(input_schema, &mut Vec::new(), true, &mut found)?;

    // Case-insensitive uniqueness, checked across the whole schema.
    for (i, param) in found.iter().enumerate() {
        if let Some(clash) = found[..i]
            .iter()
            .find(|other| other.name.eq_ignore_ascii_case(&param.name))
        {
            return Err(McpError::validation(format!(
                "duplicate {ANNOTATION} name {:?} (case-insensitively equal to {:?})",
                param.name, clash.name
            )));
        }
    }
    Ok(found)
}

fn walk(
    node: &Value,
    path: &mut Vec<String>,
    statically_reachable: bool,
    out: &mut Vec<HeaderParam>,
) -> Result<(), McpError> {
    let Some(object) = node.as_object() else {
        return Ok(());
    };

    if let Some(annotation) = object.get(ANNOTATION) {
        if !statically_reachable {
            return Err(McpError::validation(format!(
                "{ANNOTATION} on a property that is not statically reachable \
                 (reached through items, a composition or conditional keyword, or $ref)"
            )));
        }
        out.push(HeaderParam {
            name: validate_name(annotation)?,
            path: path.clone(),
        });
        validate_promotable_type(object)?;
    }

    for (key, child) in object {
        match key.as_str() {
            // The only edge that preserves static reachability.
            "properties" => {
                if let Some(properties) = child.as_object() {
                    for (name, schema) in properties {
                        path.push(name.clone());
                        let result = walk(schema, path, statically_reachable, out);
                        path.pop();
                        result?;
                    }
                }
            }
            // Everything else is off the static path. Descend anyway so an
            // annotation hiding in there is *rejected* rather than ignored.
            ANNOTATION => {}
            _ => walk_untracked(child, path, out)?,
        }
    }
    Ok(())
}

/// Recurse through non-`properties` structure, where nothing is reachable.
fn walk_untracked(
    node: &Value,
    path: &mut Vec<String>,
    out: &mut Vec<HeaderParam>,
) -> Result<(), McpError> {
    match node {
        Value::Object(_) => walk(node, path, false, out),
        Value::Array(items) => {
            for item in items {
                walk_untracked(item, path, out)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Check the annotation value is a usable HTTP field-name token.
fn validate_name(annotation: &Value) -> Result<String, McpError> {
    let name = annotation.as_str().ok_or_else(|| {
        McpError::validation(format!("{ANNOTATION} must be a string, got {annotation}"))
    })?;

    if name.is_empty() {
        return Err(McpError::validation(format!(
            "{ANNOTATION} must not be empty"
        )));
    }
    if !name.bytes().all(is_tchar) {
        return Err(McpError::validation(format!(
            "{ANNOTATION} {name:?} is not a valid HTTP field-name token"
        )));
    }
    Ok(name.to_string())
}

/// `tchar` from RFC 9110 §5.1. Excludes CR, LF and every other control
/// character by construction, so a name cannot inject a header break.
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// Only `string`, `integer` and `boolean` may be promoted.
fn validate_promotable_type(schema: &Map<String, Value>) -> Result<(), McpError> {
    let declared = schema.get("type").and_then(Value::as_str);
    match declared {
        Some("string" | "integer" | "boolean") => Ok(()),
        Some("number") => Err(McpError::validation(format!(
            "{ANNOTATION} cannot be applied to a number parameter: its text form \
             is not canonical, so header and body could disagree"
        ))),
        Some(other) => Err(McpError::validation(format!(
            "{ANNOTATION} cannot be applied to a {other} parameter; \
             only string, integer and boolean may be promoted"
        ))),
        None => Err(McpError::validation(format!(
            "{ANNOTATION} requires an explicit primitive \"type\""
        ))),
    }
}

/// Extract the header values a call's arguments supply.
///
/// A parameter absent from the arguments — or explicitly `null` — yields no
/// header, which is what the server expects. Values are returned unencoded;
/// [`crate::protocol::envelope::Envelope::push_param_header`] encodes them.
pub fn extract(
    params: &[HeaderParam],
    arguments: &Value,
) -> Result<Vec<(String, String)>, McpError> {
    let mut headers = Vec::new();
    for param in params {
        let Some(value) = value_at(arguments, &param.path) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        headers.push((param.name.clone(), stringify(&param.name, value)?));
    }
    Ok(headers)
}

/// Read the value at an exact property path, if the arguments carry one.
fn value_at<'a>(arguments: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut node = arguments;
    for step in path {
        node = node.as_object()?.get(step)?;
    }
    Some(node)
}

/// Render a promoted value as its header text form.
fn stringify(name: &str, value: &Value) -> Result<String, McpError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Number(n) => {
            let Some(i) = n.as_i64() else {
                return Err(McpError::validation(format!(
                    "{ANNOTATION} {name:?} got a non-integer number {n}"
                )));
            };
            if !(MIN_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&i) {
                return Err(McpError::validation(format!(
                    "{ANNOTATION} {name:?} value {i} is outside the safe integer range"
                )));
            }
            Ok(i.to_string())
        }
        other => Err(McpError::validation(format!(
            "{ANNOTATION} {name:?} cannot promote {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec_example() -> Value {
        // The execute_sql schema from the specification.
        json!({
            "type": "object",
            "properties": {
                "region": {
                    "type": "string",
                    "description": "The region to execute the query in",
                    "x-mcp-header": "Region"
                },
                "query": {"type": "string", "description": "The SQL query"}
            },
            "required": ["region", "query"]
        })
    }

    #[test]
    fn collects_the_specification_example() {
        let params = collect(&spec_example()).unwrap();
        assert_eq!(
            params,
            vec![HeaderParam {
                name: "Region".into(),
                path: vec!["region".into()]
            }]
        );

        let headers = extract(
            &params,
            &json!({"region": "us-west1", "query": "SELECT * FROM users"}),
        )
        .unwrap();
        assert_eq!(
            headers,
            vec![("Region".to_string(), "us-west1".to_string())]
        );
    }

    #[test]
    fn nested_properties_are_reachable() {
        let schema = json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "object",
                    "properties": {
                        "tenant": {"type": "string", "x-mcp-header": "Tenant"}
                    }
                }
            }
        });
        let params = collect(&schema).unwrap();
        assert_eq!(params[0].path, vec!["target", "tenant"]);

        let headers = extract(&params, &json!({"target": {"tenant": "acme"}})).unwrap();
        assert_eq!(headers, vec![("Tenant".to_string(), "acme".to_string())]);
    }

    #[test]
    fn unreachable_positions_are_rejected_not_ignored() {
        // Each of these hides the annotation somewhere the value's location
        // depends on the instance. Silently ignoring them would leave the
        // client sending no header while the server expects one — a -32020.
        let cases = [
            (
                "items",
                json!({"properties": {"a": {"type": "array", "items": {"type": "string", "x-mcp-header": "A"}}}}),
            ),
            (
                "oneOf",
                json!({"properties": {"a": {"oneOf": [{"type": "string", "x-mcp-header": "A"}]}}}),
            ),
            (
                "anyOf",
                json!({"properties": {"a": {"anyOf": [{"type": "string", "x-mcp-header": "A"}]}}}),
            ),
            (
                "allOf",
                json!({"properties": {"a": {"allOf": [{"type": "string", "x-mcp-header": "A"}]}}}),
            ),
            (
                "not",
                json!({"properties": {"a": {"not": {"type": "string", "x-mcp-header": "A"}}}}),
            ),
            (
                "if",
                json!({"properties": {"a": {"if": {"type": "string", "x-mcp-header": "A"}}}}),
            ),
            (
                "then",
                json!({"properties": {"a": {"then": {"type": "string", "x-mcp-header": "A"}}}}),
            ),
            (
                "$defs",
                json!({"$defs": {"a": {"type": "string", "x-mcp-header": "A"}}}),
            ),
        ];
        for (label, schema) in cases {
            let err = collect(&schema).expect_err(&format!("{label} should be rejected"));
            assert!(
                err.to_string().contains("statically reachable"),
                "{label}: {err}"
            );
        }
    }

    #[test]
    fn invalid_names_are_rejected() {
        for (label, name) in [
            ("empty", json!("")),
            ("space", json!("My Header")),
            ("colon", json!("X:Y")),
            ("crlf", json!("X\r\nInjected: 1")),
            ("newline", json!("X\nY")),
            ("control", json!("X\u{0007}")),
            ("non-ascii", json!("Región")),
            ("not a string", json!(42)),
        ] {
            let schema = json!({"properties": {"p": {"type": "string", ANNOTATION: name}}});
            assert!(
                collect(&schema).is_err(),
                "{label} should be rejected as a header name"
            );
        }
    }

    #[test]
    fn valid_token_characters_are_accepted() {
        let schema = json!({
            "properties": {"p": {"type": "string", ANNOTATION: "A-b_1.2!#$%&'*+^`|~"}}
        });
        assert_eq!(collect(&schema).unwrap().len(), 1);
    }

    #[test]
    fn only_primitive_types_may_be_promoted() {
        for ty in ["string", "integer", "boolean"] {
            let schema = json!({"properties": {"p": {"type": ty, ANNOTATION: "P"}}});
            assert_eq!(collect(&schema).unwrap().len(), 1, "{ty} should be allowed");
        }

        // `number` is called out explicitly by the spec.
        let number = json!({"properties": {"p": {"type": "number", ANNOTATION: "P"}}});
        let err = collect(&number).unwrap_err();
        assert!(err.to_string().contains("number"), "{err}");

        for ty in ["object", "array", "null"] {
            let schema = json!({"properties": {"p": {"type": ty, ANNOTATION: "P"}}});
            assert!(collect(&schema).is_err(), "{ty} should be rejected");
        }

        // No declared type at all: nothing to verify against.
        let untyped = json!({"properties": {"p": {ANNOTATION: "P"}}});
        assert!(collect(&untyped).is_err());
    }

    #[test]
    fn duplicate_names_are_rejected_case_insensitively() {
        let schema = json!({
            "properties": {
                "a": {"type": "string", ANNOTATION: "Tenant"},
                "b": {"type": "string", ANNOTATION: "TENANT"}
            }
        });
        let err = collect(&schema).unwrap_err();
        assert!(err.to_string().contains("duplicate"), "{err}");

        // Distinct names are fine.
        let ok = json!({
            "properties": {
                "a": {"type": "string", ANNOTATION: "Tenant"},
                "b": {"type": "string", ANNOTATION: "Region"}
            }
        });
        assert_eq!(collect(&ok).unwrap().len(), 2);
    }

    #[test]
    fn absent_and_null_values_omit_the_header() {
        let params = collect(&json!({
            "properties": {
                "region": {"type": "string", ANNOTATION: "Region"},
                "tenant": {"type": "string", ANNOTATION: "Tenant"}
            }
        }))
        .unwrap();

        // Only `region` supplied; `tenant` explicitly null.
        let headers = extract(&params, &json!({"region": "eu-west1", "tenant": null})).unwrap();
        assert_eq!(
            headers,
            vec![("Region".to_string(), "eu-west1".to_string())]
        );

        // Nothing supplied at all.
        assert!(extract(&params, &json!({})).unwrap().is_empty());
        // Arguments that are not even an object.
        assert!(extract(&params, &json!(null)).unwrap().is_empty());
    }

    #[test]
    fn value_type_conversion() {
        let params = collect(&json!({
            "properties": {
                "s": {"type": "string", ANNOTATION: "S"},
                "i": {"type": "integer", ANNOTATION: "I"},
                "b": {"type": "boolean", ANNOTATION: "B"}
            }
        }))
        .unwrap();

        // Order follows schema key order (deterministic, but not significant —
        // HTTP header order carries no meaning), so compare as a sorted set.
        let mut headers = extract(&params, &json!({"s": "x", "i": -7, "b": true})).unwrap();
        headers.sort();
        assert_eq!(
            headers,
            vec![
                // Lowercase boolean, per the spec.
                ("B".to_string(), "true".to_string()),
                ("I".to_string(), "-7".to_string()),
                ("S".to_string(), "x".to_string()),
            ]
        );

        let f = extract(&params, &json!({"b": false})).unwrap();
        assert_eq!(f, vec![("B".to_string(), "false".to_string())]);
    }

    #[test]
    fn integers_outside_the_safe_range_are_rejected() {
        let params =
            collect(&json!({"properties": {"i": {"type": "integer", ANNOTATION: "I"}}})).unwrap();

        for ok in [MAX_SAFE_INTEGER, MIN_SAFE_INTEGER, 0] {
            assert!(extract(&params, &json!({"i": ok})).is_ok(), "{ok}");
        }
        for bad in [MAX_SAFE_INTEGER + 1, MIN_SAFE_INTEGER - 1, i64::MAX] {
            let err = extract(&params, &json!({"i": bad})).unwrap_err();
            assert!(
                err.to_string().contains("safe integer range"),
                "{bad}: {err}"
            );
        }
        // A float where an integer was declared.
        assert!(extract(&params, &json!({"i": 1.5})).is_err());
    }

    #[test]
    fn malformed_schemas_are_tolerated_not_panicked_on() {
        // A tool definition is untrusted input. Nonsense in the schema shape
        // must yield "no promotions" rather than bring the client down.
        for schema in [
            json!({"properties": "not an object"}),
            json!({"properties": 42}),
            json!({"properties": null}),
            json!({"properties": []}),
            json!({"properties": {"p": "not an object"}}),
        ] {
            assert!(collect(&schema).unwrap().is_empty(), "{schema}");
        }
    }

    #[test]
    fn arguments_that_contradict_the_declared_type_are_rejected() {
        // The schema promised a string; the call supplied a container. Sending
        // some stringification would risk a header that cannot match the body.
        let params =
            collect(&json!({"properties": {"p": {"type": "string", ANNOTATION: "P"}}})).unwrap();

        for bad in [json!({"p": ["a"]}), json!({"p": {"nested": 1}})] {
            let err = extract(&params, &bad).unwrap_err();
            assert!(err.to_string().contains("cannot promote"), "{bad}: {err}");
        }
    }

    #[test]
    fn schemas_without_annotations_yield_nothing() {
        for schema in [
            json!({}),
            json!({"type": "object"}),
            json!({"type": "object", "properties": {"a": {"type": "string"}}}),
            json!("not even an object"),
            json!(null),
        ] {
            assert!(collect(&schema).unwrap().is_empty());
        }
    }
}
