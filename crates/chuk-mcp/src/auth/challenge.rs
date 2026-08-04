//! Reading a `WWW-Authenticate` challenge.
//!
//! A protected MCP server answers an unauthenticated request with `401` and a
//! `Bearer` challenge, and an under-scoped one with `403` and the same header
//! carrying `error="insufficient_scope"`. Both are the client's instructions:
//! where the resource metadata lives, and which scopes this operation needs.
//!
//! The parsing is deliberately tolerant of layout and strict about nothing
//! else. Header field values may be split across lines, quoted or bare, and
//! separated by commas with arbitrary whitespace — none of which changes their
//! meaning, and all of which appears in the wild.

use std::collections::BTreeMap;

/// The parameters of a `Bearer` challenge.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Challenge {
    /// Where the protected resource metadata document lives, if the server
    /// said. Its presence is what lets a client skip well-known probing.
    pub resource_metadata: Option<String>,
    /// The scopes this operation needs, space-separated as sent.
    ///
    /// Authoritative for the current request: a client **MUST** treat these as
    /// what is required now, whatever `scopes_supported` says.
    pub scope: Option<String>,
    /// `invalid_token`, `insufficient_scope`, and so on.
    pub error: Option<String>,
    /// Human-readable elaboration, for logs and error messages.
    pub error_description: Option<String>,
    /// Everything else the challenge carried, for callers that need it.
    pub extra: BTreeMap<String, String>,
}

impl Challenge {
    /// The scopes this challenge asked for, split on whitespace.
    pub fn scopes(&self) -> Vec<String> {
        self.scope
            .as_deref()
            .map(|scope| scope.split_whitespace().map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// Whether this challenge says the token was fine but too narrow.
    pub fn is_insufficient_scope(&self) -> bool {
        self.error.as_deref() == Some("insufficient_scope")
    }
}

/// Parse a `WWW-Authenticate` header value.
///
/// Returns `None` when the header names no scheme this client can answer. Only
/// `Bearer` is understood: a `Basic` or `Negotiate` challenge is not something
/// an MCP authorization flow can satisfy, and treating it as one would send a
/// user through an OAuth dance that could never work.
pub fn parse(header: &str) -> Option<Challenge> {
    let rest = strip_scheme(header, "Bearer")?;

    let mut challenge = Challenge::default();
    for (key, value) in parameters(rest) {
        match key.as_str() {
            "resource_metadata" => challenge.resource_metadata = Some(value),
            "scope" => challenge.scope = Some(value),
            "error" => challenge.error = Some(value),
            "error_description" => challenge.error_description = Some(value),
            _ => {
                challenge.extra.insert(key, value);
            }
        }
    }
    Some(challenge)
}

/// Strip a scheme name from the front of a challenge, case-insensitively.
///
/// The scheme is a token, so it ends at the first space; comparing without
/// case folding would reject the perfectly legal `bearer`.
fn strip_scheme<'a>(header: &'a str, scheme: &str) -> Option<&'a str> {
    let header = header.trim_start();
    let (named, rest) = match header.find(char::is_whitespace) {
        Some(at) => header.split_at(at),
        // A bare scheme with no parameters is still a challenge.
        None => (header, ""),
    };
    named.eq_ignore_ascii_case(scheme).then_some(rest)
}

/// Split `key=value` pairs, honouring quoted values.
///
/// Hand-rolled rather than split-on-comma because a quoted value may itself
/// contain a comma — `error_description="failed, try again"` is one parameter,
/// not two, and splitting first would corrupt it.
fn parameters(input: &str) -> Vec<(String, String)> {
    let mut found = Vec::new();
    let mut chars = input.chars().peekable();

    loop {
        // Skip separators between parameters.
        while chars.peek().is_some_and(|c| c.is_whitespace() || *c == ',') {
            chars.next();
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() || c == ',' {
                break;
            }
            key.push(c);
            chars.next();
        }
        if key.is_empty() {
            return found;
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        // A parameter with no value is not one this client can use, but it
        // must not swallow the parameters after it.
        if chars.peek() != Some(&'=') {
            continue;
        }
        chars.next();
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }

        let mut value = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    // A backslash escapes the character after it, so a quote
                    // inside a value does not end it.
                    '\\' => {
                        if let Some(escaped) = chars.next() {
                            value.push(escaped);
                        }
                    }
                    '"' => break,
                    _ => value.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ',' || c.is_whitespace() {
                    break;
                }
                value.push(c);
                chars.next();
            }
        }
        found.push((key.to_ascii_lowercase(), value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_specification_example_parses() {
        let challenge = parse(
            r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="files:read""#,
        )
        .expect("a Bearer challenge");

        assert_eq!(
            challenge.resource_metadata.as_deref(),
            Some("https://mcp.example.com/.well-known/oauth-protected-resource")
        );
        assert_eq!(challenge.scopes(), vec!["files:read"]);
    }

    #[test]
    fn an_insufficient_scope_challenge_is_recognised() {
        let challenge = parse(
            r#"Bearer error="insufficient_scope", scope="files:write files:read", resource_metadata="https://x/.well-known/oauth-protected-resource", error_description="File write permission required""#,
        )
        .unwrap();

        assert!(challenge.is_insufficient_scope());
        assert_eq!(challenge.scopes(), vec!["files:write", "files:read"]);
        assert_eq!(
            challenge.error_description.as_deref(),
            Some("File write permission required")
        );
    }

    /// Auth-scheme names are case-insensitive per RFC 9110.
    #[test]
    fn the_scheme_is_matched_without_regard_to_case() {
        assert!(parse("bearer scope=\"a\"").is_some());
        assert!(parse("BEARER scope=\"a\"").is_some());
        assert!(parse("Bearer").is_some());
    }

    /// A scheme this client cannot satisfy must not be mistaken for one it
    /// can — an OAuth flow would never answer a `Basic` challenge.
    #[test]
    fn another_scheme_is_not_a_bearer_challenge() {
        assert_eq!(parse("Basic realm=\"x\""), None);
        assert_eq!(parse("Negotiate"), None);
    }

    #[test]
    fn unquoted_values_are_read_too() {
        let challenge = parse("Bearer error=invalid_token, scope=a b").unwrap();
        assert_eq!(challenge.error.as_deref(), Some("invalid_token"));
        // An unquoted value ends at whitespace, so only the first token is the
        // scope — which is why a multi-scope value has to be quoted.
        assert_eq!(challenge.scope.as_deref(), Some("a"));
    }

    /// The reason this is not a split on commas.
    #[test]
    fn a_comma_inside_a_quoted_value_does_not_end_it() {
        let challenge =
            parse(r#"Bearer error_description="failed, try again", error="x""#).unwrap();
        assert_eq!(
            challenge.error_description.as_deref(),
            Some("failed, try again")
        );
        assert_eq!(challenge.error.as_deref(), Some("x"));
    }

    #[test]
    fn an_escaped_quote_stays_inside_the_value() {
        let challenge = parse(r#"Bearer error_description="say \"hello\"""#).unwrap();
        assert_eq!(
            challenge.error_description.as_deref(),
            Some(r#"say "hello""#)
        );
    }

    /// Parameter names are case-insensitive, so a server shouting them still
    /// gets understood.
    #[test]
    fn parameter_names_are_lowercased() {
        let challenge = parse(r#"Bearer Resource_Metadata="https://x", SCOPE="a""#).unwrap();
        assert_eq!(challenge.resource_metadata.as_deref(), Some("https://x"));
        assert_eq!(challenge.scopes(), vec!["a"]);
    }

    #[test]
    fn unknown_parameters_are_kept_rather_than_dropped() {
        let challenge = parse(r#"Bearer realm="mcp", scope="a""#).unwrap();
        assert_eq!(
            challenge.extra.get("realm").map(String::as_str),
            Some("mcp")
        );
    }

    /// A valueless parameter must not eat the ones after it.
    #[test]
    fn a_parameter_without_a_value_is_skipped_not_fatal() {
        let challenge = parse(r#"Bearer broken, scope="a""#).unwrap();
        assert_eq!(challenge.scopes(), vec!["a"]);
    }

    #[test]
    fn a_challenge_with_no_scope_asks_for_none() {
        let challenge = parse("Bearer").unwrap();
        assert!(challenge.scopes().is_empty());
        assert!(!challenge.is_insufficient_scope());
    }
}
