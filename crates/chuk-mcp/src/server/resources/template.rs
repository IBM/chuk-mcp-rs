//! Matching a templated resource URI such as `db://{table}/rows/{id}`.

use std::collections::BTreeMap;

/// One piece of a URI template: text that must match, or a name to capture.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Piece {
    Literal(String),
    Variable(String),
}

/// A parsed `test://thing/{id}/data`.
///
/// Only simple string expansion is supported — `{name}` standing for one path
/// segment — which is what the specification's resource templates use. An
/// unclosed brace is treated as literal text rather than silently swallowing
/// the rest of the pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UriTemplate {
    pieces: Vec<Piece>,
    source: String,
}

impl UriTemplate {
    /// Parse a template. Never fails: anything not recognisable as a variable
    /// is literal text, so a plain URI is a template that matches only itself.
    pub fn parse(pattern: &str) -> Self {
        let mut pieces = Vec::new();
        let mut rest = pattern;

        while let Some(open) = rest.find('{') {
            match rest[open..].find('}') {
                Some(close) => {
                    let close = open + close;
                    if open > 0 {
                        pieces.push(Piece::Literal(rest[..open].to_string()));
                    }
                    pieces.push(Piece::Variable(rest[open + 1..close].to_string()));
                    rest = &rest[close + 1..];
                }
                // No closing brace: the brace is just a character.
                None => break,
            }
        }
        if !rest.is_empty() {
            pieces.push(Piece::Literal(rest.to_string()));
        }

        UriTemplate {
            pieces,
            source: pattern.to_string(),
        }
    }

    /// The template as written, for `resources/templates/list`.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// The variables `uri` binds, or `None` if it does not match.
    ///
    /// A variable never spans `/`: `test://a/{id}/b` describes one segment, and
    /// letting `{id}` swallow slashes would make it match URIs it does not
    /// describe.
    pub fn match_uri(&self, uri: &str) -> Option<BTreeMap<String, String>> {
        let mut bound = BTreeMap::new();
        let mut rest = uri;
        let mut pieces = self.pieces.iter().peekable();

        while let Some(piece) = pieces.next() {
            match piece {
                Piece::Literal(literal) => {
                    rest = rest.strip_prefix(literal.as_str())?;
                }
                Piece::Variable(name) => {
                    // How far this variable runs: up to the next literal, or to
                    // the end when it is the last piece.
                    let value = match pieces.peek() {
                        Some(Piece::Literal(next)) => {
                            let end = rest.find(next.as_str())?;
                            &rest[..end]
                        }
                        // Two variables with nothing between them cannot be
                        // told apart, so the pattern is not usable.
                        Some(Piece::Variable(_)) => return None,
                        None => rest,
                    };
                    if value.is_empty() || value.contains('/') {
                        return None;
                    }
                    bound.insert(name.clone(), value.to_string());
                    rest = &rest[value.len()..];
                }
            }
        }

        rest.is_empty().then_some(bound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_template_binds_the_variable_it_names() {
        let template = UriTemplate::parse("test://template/{id}/data");
        assert_eq!(template.as_str(), "test://template/{id}/data");

        let bound = template
            .match_uri("test://template/123/data")
            .expect("a match");
        assert_eq!(bound.get("id").map(String::as_str), Some("123"));
    }

    #[test]
    fn a_uri_the_template_does_not_describe_does_not_match() {
        let template = UriTemplate::parse("test://template/{id}/data");
        assert!(template.match_uri("test://template/123/other").is_none());
        assert!(template.match_uri("test://other/123/data").is_none());
        // An empty variable is not a binding.
        assert!(template.match_uri("test://template//data").is_none());
        // A variable is one segment, so this is a different URI.
        assert!(template.match_uri("test://template/1/2/data").is_none());
    }

    #[test]
    fn several_variables_each_bind() {
        let template = UriTemplate::parse("db://{table}/rows/{id}");
        let bound = template.match_uri("db://users/rows/7").expect("a match");
        assert_eq!(bound.get("table").map(String::as_str), Some("users"));
        assert_eq!(bound.get("id").map(String::as_str), Some("7"));
    }

    #[test]
    fn a_trailing_variable_runs_to_the_end() {
        let template = UriTemplate::parse("file://{name}");
        let bound = template.match_uri("file://notes.txt").expect("a match");
        assert_eq!(bound.get("name").map(String::as_str), Some("notes.txt"));
    }

    #[test]
    fn a_template_with_no_variables_matches_only_itself() {
        let template = UriTemplate::parse("test://static");
        assert!(template.match_uri("test://static").is_some());
        assert!(template.match_uri("test://static/more").is_none());
    }

    #[test]
    fn an_unclosed_brace_is_literal_text_not_a_swallowed_pattern() {
        let template = UriTemplate::parse("test://{unclosed");
        assert!(template.match_uri("test://{unclosed").is_some());
        assert!(template.match_uri("test://anything").is_none());
    }

    #[test]
    fn adjacent_variables_match_nothing_rather_than_guessing() {
        // "{a}{b}" has no boundary to split on; any answer would be invented.
        let template = UriTemplate::parse("x://{a}{b}");
        assert!(template.match_uri("x://onetwo").is_none());
    }
}
