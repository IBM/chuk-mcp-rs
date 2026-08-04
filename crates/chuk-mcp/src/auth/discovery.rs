//! Finding the authorization server, and proving it is the one that was named.
//!
//! Two hops. The MCP server publishes *protected resource metadata* (RFC 9728)
//! naming its authorization servers; each authorization server publishes its
//! own metadata (RFC 8414, or OpenID Connect Discovery) naming its endpoints.
//!
//! Both hops are attacker-reachable, so both are validated:
//!
//! * The resource metadata's `resource` must match the server being talked to.
//!   Without that check a server could name an authorization server for a
//!   *different* resource and collect tokens minted for it.
//! * The authorization server metadata's `issuer` must equal the issuer the
//!   URL was built from. RFC 8414 §3.3 requires this, and the reason is
//!   direct: a document fetched from `attacker.example` claiming
//!   `"issuer": "honest.example"` would otherwise redirect the whole flow.
//!
//! The candidate URL orders below are not arbitrary — they are the sequences
//! the specification requires, and a client that tries them in another order
//! can land on the wrong document on a server that publishes several.

use reqwest::Url;
use serde::Deserialize;

use crate::protocol::types::errors::McpError;

/// The `.well-known` suffix RFC 9728 defines for resource metadata.
const PRM_SUFFIX: &str = "/.well-known/oauth-protected-resource";
/// The suffix RFC 8414 defines for authorization server metadata.
const AS_SUFFIX: &str = "/.well-known/oauth-authorization-server";
/// The path OpenID Connect Discovery 1.0 defines.
const OIDC_SUFFIX: &str = "/.well-known/openid-configuration";

/// An MCP server's protected resource metadata (RFC 9728).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ProtectedResourceMetadata {
    /// The canonical URI of the resource this document describes.
    pub resource: String,
    /// The authorization servers that can mint tokens for it.
    #[serde(default)]
    pub authorization_servers: Vec<String>,
    /// The scopes the resource understands, if it says.
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
}

/// An authorization server's metadata (RFC 8414 / OpenID Connect Discovery).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AuthorizationServerMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
    #[serde(default)]
    pub scopes_supported: Option<Vec<String>>,
    #[serde(default)]
    pub code_challenge_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub token_endpoint_auth_methods_supported: Option<Vec<String>>,
    #[serde(default)]
    pub grant_types_supported: Option<Vec<String>>,
    /// Whether this server promises to send `iss` on authorization responses.
    ///
    /// Absent and `false` mean the same thing here, and the difference from
    /// `true` decides whether a *missing* `iss` is fatal — see
    /// [`super::flow`].
    #[serde(default)]
    pub authorization_response_iss_parameter_supported: Option<bool>,
    /// Whether the server accepts an HTTPS URL as a `client_id`.
    #[serde(default)]
    pub client_id_metadata_document_supported: Option<bool>,
}

impl AuthorizationServerMetadata {
    /// Whether the server advertises support for a token endpoint auth method.
    pub fn supports_auth_method(&self, method: &str) -> bool {
        self.token_endpoint_auth_methods_supported
            .as_ref()
            .is_some_and(|methods| methods.iter().any(|m| m == method))
    }

    /// Whether `scopes_supported` names a scope.
    pub fn offers_scope(&self, scope: &str) -> bool {
        self.scopes_supported
            .as_ref()
            .is_some_and(|scopes| scopes.iter().any(|s| s == scope))
    }
}

/// The resource metadata URLs to try for an MCP server, in order.
///
/// The path-specific location comes first. A server at `/mcp` may publish at
/// `/.well-known/oauth-protected-resource/mcp` *and* have an unrelated
/// document at the root; asking the root first would find the wrong one.
pub fn resource_metadata_candidates(server_url: &str) -> Result<Vec<String>, McpError> {
    let url = parse_url(server_url)?;
    let origin = origin_of(&url);
    let path = url.path().trim_end_matches('/');

    let mut candidates = Vec::new();
    if !path.is_empty() {
        candidates.push(format!("{origin}{PRM_SUFFIX}{path}"));
    }
    candidates.push(format!("{origin}{PRM_SUFFIX}"));
    Ok(candidates)
}

/// The authorization server metadata URLs to try for an issuer, in order.
///
/// The three-candidate form for an issuer with a path is what makes a
/// tenant-scoped authorization server discoverable: RFC 8414 *inserts* the
/// well-known segment before the path, OpenID Connect Discovery *appends* it,
/// and real deployments do both.
pub fn server_metadata_candidates(issuer: &str) -> Result<Vec<String>, McpError> {
    let url = parse_url(issuer)?;
    let origin = origin_of(&url);
    let path = url.path().trim_end_matches('/');

    Ok(if path.is_empty() {
        vec![
            format!("{origin}{AS_SUFFIX}"),
            format!("{origin}{OIDC_SUFFIX}"),
        ]
    } else {
        vec![
            format!("{origin}{AS_SUFFIX}{path}"),
            format!("{origin}{OIDC_SUFFIX}{path}"),
            format!("{origin}{path}{OIDC_SUFFIX}"),
        ]
    })
}

/// Whether a resource metadata document describes the server being talked to.
///
/// The document must **cover** the server: same origin, and a path the
/// server's own path sits under. Exact equality is the common case — a
/// document at `/.well-known/oauth-protected-resource/mcp` names
/// `https://host/mcp` — but a server may also publish one document at its root
/// describing everything it hosts, in which case the declared resource is the
/// bare origin and the endpoint at `/mcp` is inside it.
///
/// Coverage rather than equality, and *not* mere prefix matching: the boundary
/// has to fall on a path segment, or `https://host/mcp-admin` would be treated
/// as covered by a document for `https://host/mcp`.
///
/// The origin is compared case-insensitively on scheme and host, which
/// [RFC 3986 §6.2.2.1] makes equivalent. Nothing else is normalised. A
/// different host is a different resource, which is exactly the substitution
/// this check exists to refuse.
///
/// [RFC 3986 §6.2.2.1]: https://datatracker.ietf.org/doc/html/rfc3986#section-6.2.2.1
pub fn resource_matches(declared: &str, server_url: &str) -> bool {
    let (Ok(declared), Ok(actual)) = (parse_url(declared), parse_url(server_url)) else {
        return false;
    };
    let same_origin = declared.scheme().eq_ignore_ascii_case(actual.scheme())
        && declared.host_str().map(str::to_lowercase) == actual.host_str().map(str::to_lowercase)
        && declared.port_or_known_default() == actual.port_or_known_default();
    if !same_origin {
        return false;
    }

    let declared_path = declared.path().trim_end_matches('/');
    let actual_path = actual.path().trim_end_matches('/');
    declared_path.is_empty()
        || declared_path == actual_path
        || actual_path.starts_with(&format!("{declared_path}/"))
}

/// Whether a metadata document's issuer is the one its URL was built from.
///
/// Simple string comparison, as RFC 8414 §3.3 requires. Normalising first
/// would let `https://as.example/` pass for `https://as.example` — harmless
/// here, but the same leniency applied to the `iss` check later is not, and
/// having one rule for both is what keeps them consistent.
pub fn issuer_matches(declared: &str, expected: &str) -> bool {
    declared == expected
}

/// The canonical resource identifier to send as the `resource` parameter.
///
/// RFC 8707 wants the most specific URI identifying the server, without a
/// fragment. The query string goes too: it identifies a request, not a
/// resource.
pub fn canonical_resource(server_url: &str) -> Result<String, McpError> {
    let mut url = parse_url(server_url)?;
    url.set_fragment(None);
    url.set_query(None);

    let canonical = url.to_string();
    // A bare origin renders with a trailing slash that the specification asks
    // implementations to drop for interoperability.
    Ok(match canonical.strip_suffix('/') {
        Some(trimmed) if url.path() == "/" => trimmed.to_string(),
        _ => canonical,
    })
}

fn parse_url(raw: &str) -> Result<Url, McpError> {
    Url::parse(raw).map_err(|e| McpError::validation(format!("invalid URL {raw:?}: {e}")))
}

/// Scheme, host and port, with no path.
fn origin_of(url: &Url) -> String {
    let mut origin = format!("{}://{}", url.scheme(), url.host_str().unwrap_or_default());
    if let Some(port) = url.port() {
        origin.push_str(&format!(":{port}"));
    }
    origin
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_server_with_a_path_is_asked_about_its_path_first() {
        let candidates = resource_metadata_candidates("https://mcp.example.com/mcp").unwrap();
        assert_eq!(
            candidates,
            vec![
                "https://mcp.example.com/.well-known/oauth-protected-resource/mcp",
                "https://mcp.example.com/.well-known/oauth-protected-resource",
            ]
        );
    }

    /// A server at the root has no path-specific location to ask about.
    #[test]
    fn a_root_server_has_only_the_root_candidate() {
        let candidates = resource_metadata_candidates("https://mcp.example.com").unwrap();
        assert_eq!(
            candidates,
            vec!["https://mcp.example.com/.well-known/oauth-protected-resource"]
        );
    }

    #[test]
    fn a_port_is_carried_into_the_candidates() {
        let candidates = resource_metadata_candidates("http://localhost:8931/mcp").unwrap();
        assert_eq!(
            candidates[0],
            "http://localhost:8931/.well-known/oauth-protected-resource/mcp"
        );
    }

    /// The exact order the specification lists for a path-bearing issuer.
    #[test]
    fn an_issuer_with_a_path_gets_all_three_candidates_in_order() {
        let candidates = server_metadata_candidates("https://auth.example.com/tenant1").unwrap();
        assert_eq!(
            candidates,
            vec![
                "https://auth.example.com/.well-known/oauth-authorization-server/tenant1",
                "https://auth.example.com/.well-known/openid-configuration/tenant1",
                "https://auth.example.com/tenant1/.well-known/openid-configuration",
            ]
        );
    }

    #[test]
    fn an_issuer_without_a_path_gets_the_two_root_candidates() {
        let candidates = server_metadata_candidates("https://auth.example.com").unwrap();
        assert_eq!(
            candidates,
            vec![
                "https://auth.example.com/.well-known/oauth-authorization-server",
                "https://auth.example.com/.well-known/openid-configuration",
            ]
        );
    }

    /// A trailing slash does not make an issuer path-bearing.
    #[test]
    fn a_trailing_slash_is_not_a_path() {
        assert_eq!(
            server_metadata_candidates("https://auth.example.com/").unwrap(),
            server_metadata_candidates("https://auth.example.com").unwrap()
        );
    }

    /// A document at the server's root covers the endpoints beneath it, which
    /// is how a server that publishes one PRM for everything is read.
    #[test]
    fn a_root_document_covers_a_sub_path_endpoint() {
        assert!(resource_matches(
            "https://mcp.example.com",
            "https://mcp.example.com/mcp"
        ));
        assert!(resource_matches(
            "https://mcp.example.com/",
            "https://mcp.example.com/mcp"
        ));
        assert!(resource_matches(
            "https://mcp.example.com/servers",
            "https://mcp.example.com/servers/one"
        ));
    }

    /// Coverage stops at a segment boundary: `/mcp` does not cover
    /// `/mcp-admin`, however alike the two strings look.
    #[test]
    fn coverage_does_not_leak_across_a_partial_segment() {
        assert!(!resource_matches(
            "https://mcp.example.com/mcp",
            "https://mcp.example.com/mcp-admin"
        ));
    }

    /// A narrower document does not describe a broader server.
    #[test]
    fn a_deeper_document_does_not_cover_a_shallower_server() {
        assert!(!resource_matches(
            "https://mcp.example.com/mcp/inner",
            "https://mcp.example.com/mcp"
        ));
    }

    #[test]
    fn a_resource_matches_itself_and_its_trailing_slash() {
        assert!(resource_matches(
            "https://mcp.example.com/mcp",
            "https://mcp.example.com/mcp"
        ));
        assert!(resource_matches(
            "https://mcp.example.com/mcp/",
            "https://mcp.example.com/mcp"
        ));
        // Scheme and host are case-insensitive.
        assert!(resource_matches(
            "HTTPS://MCP.EXAMPLE.COM/mcp",
            "https://mcp.example.com/mcp"
        ));
    }

    /// The check that stops a server naming an authorization server for
    /// somebody else's resource.
    #[test]
    fn a_different_resource_does_not_match() {
        assert!(!resource_matches(
            "https://other.example.com/mcp",
            "https://mcp.example.com/mcp"
        ));
        assert!(!resource_matches(
            "https://mcp.example.com/other",
            "https://mcp.example.com/mcp"
        ));
        // The case the conformance suite checks: a document naming another
        // host entirely must never be accepted.
        assert!(!resource_matches(
            "https://evil.example.com/mcp",
            "http://localhost:8931/mcp"
        ));
        assert!(!resource_matches("not a url", "https://mcp.example.com"));
    }

    /// RFC 8414 §3.3: the document must name the issuer it was fetched for.
    #[test]
    fn issuer_comparison_is_exact() {
        assert!(issuer_matches("https://as.example", "https://as.example"));
        assert!(!issuer_matches("https://as.example/", "https://as.example"));
        assert!(!issuer_matches(
            "https://evil.example",
            "https://as.example"
        ));
    }

    #[test]
    fn a_canonical_resource_drops_the_fragment_and_query() {
        assert_eq!(
            canonical_resource("https://mcp.example.com/mcp?x=1#frag").unwrap(),
            "https://mcp.example.com/mcp"
        );
    }

    /// The specification asks for the form without the trailing slash.
    #[test]
    fn a_bare_origin_canonicalises_without_a_trailing_slash() {
        assert_eq!(
            canonical_resource("https://mcp.example.com").unwrap(),
            "https://mcp.example.com"
        );
        assert_eq!(
            canonical_resource("https://mcp.example.com/").unwrap(),
            "https://mcp.example.com"
        );
        // A real path keeps its shape.
        assert_eq!(
            canonical_resource("https://mcp.example.com/mcp").unwrap(),
            "https://mcp.example.com/mcp"
        );
    }

    #[test]
    fn metadata_accessors_read_what_the_server_advertised() {
        let metadata: AuthorizationServerMetadata = serde_json::from_value(serde_json::json!({
            "issuer": "https://as.example",
            "authorization_endpoint": "https://as.example/authorize",
            "token_endpoint": "https://as.example/token",
            "scopes_supported": ["files:read", "offline_access"],
            "token_endpoint_auth_methods_supported": ["client_secret_post"],
        }))
        .unwrap();

        assert!(metadata.offers_scope("offline_access"));
        assert!(!metadata.offers_scope("files:write"));
        assert!(metadata.supports_auth_method("client_secret_post"));
        assert!(!metadata.supports_auth_method("client_secret_basic"));
        // Absent means "not advertised", which the flow treats as false.
        assert_eq!(
            metadata.authorization_response_iss_parameter_supported,
            None
        );
    }

    #[test]
    fn resource_metadata_parses_with_optional_fields_absent() {
        let metadata: ProtectedResourceMetadata = serde_json::from_value(serde_json::json!({
            "resource": "https://mcp.example.com/mcp",
            "authorization_servers": ["https://as.example"],
        }))
        .unwrap();

        assert_eq!(metadata.scopes_supported, None);
        assert_eq!(metadata.authorization_servers.len(), 1);
    }
}
