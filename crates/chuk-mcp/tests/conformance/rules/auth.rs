//! What OAuth 2.1 obliges our client to do.
//!
//! These are the rules where getting it *nearly* right is the dangerous case.
//! A discovery order that is merely plausible finds the wrong document on a
//! server that publishes several; an `iss` comparison that normalises first
//! accepts the substitution it exists to catch; a step-up that takes the
//! challenge at face value silently drops permissions granted earlier.
//!
//! Everything here is checked without a network: these are decisions about
//! what to send and what to accept, and a rule that needed a live
//! authorization server to state them would be testing the server instead.

use chuk_mcp::auth::challenge;
use chuk_mcp::auth::discovery::{
    self, resource_matches, server_metadata_candidates, AuthorizationServerMetadata,
};
use chuk_mcp::auth::flow::{union_scopes, validate_iss};
use chuk_mcp::auth::pkce::Pkce;

use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::sync(
            "modern.client.auth-prm-path-before-root",
            Era::Modern,
            Subject::Client,
            "Protected resource metadata is sought at the server's path before the root",
            prm_path_before_root,
        ),
        Rule::sync(
            "modern.client.auth-as-discovery-order",
            Era::Modern,
            Subject::Client,
            "Authorization server metadata is sought in the specification's exact order",
            as_discovery_order,
        ),
        Rule::sync(
            "modern.client.auth-issuer-must-match",
            Era::Modern,
            Subject::Client,
            "Metadata whose `issuer` differs from the URL it was fetched for is refused",
            issuer_must_match,
        ),
        Rule::sync(
            "modern.client.auth-resource-must-cover-server",
            Era::Modern,
            Subject::Client,
            "Resource metadata for a different resource is refused before authorizing",
            resource_must_cover_server,
        ),
        Rule::sync(
            "modern.client.auth-iss-validation-table",
            Era::Modern,
            Subject::Client,
            "RFC 9207 `iss` validation follows the specification's four-case table",
            iss_validation_table,
        ),
        Rule::sync(
            "modern.client.auth-iss-not-normalised",
            Era::Modern,
            Subject::Client,
            "`iss` is compared by simple string comparison, with no URL normalisation",
            iss_is_not_normalised,
        ),
        Rule::sync(
            "modern.client.auth-step-up-unions-scopes",
            Era::Modern,
            Subject::Client,
            "A step-up requests the union of held and challenged scopes, not the challenge alone",
            step_up_unions_scopes,
        ),
        Rule::sync(
            "modern.client.auth-pkce-s256",
            Era::Modern,
            Subject::Client,
            "PKCE challenges are S256 over a verifier of the required length",
            pkce_is_s256,
        ),
        Rule::sync(
            "modern.client.auth-challenge-parsing",
            Era::Modern,
            Subject::Client,
            "A `WWW-Authenticate` Bearer challenge is parsed, including quoted commas",
            challenge_parsing,
        ),
    ]
}

/// Metadata for `https://as.example`, with `extra` merged over it.
fn metadata(extra: serde_json::Value) -> AuthorizationServerMetadata {
    let mut base = serde_json::json!({
        "issuer": "https://as.example",
        "authorization_endpoint": "https://as.example/authorize",
        "token_endpoint": "https://as.example/token",
    });
    for (key, value) in extra.as_object().expect("an object") {
        base[key] = value.clone();
    }
    serde_json::from_value(base).expect("metadata")
}

fn prm_path_before_root() -> Verdict {
    let candidates = discovery::resource_metadata_candidates("https://mcp.example.com/mcp")
        .map_err(|e| e.to_string())?;

    // A server may host an unrelated document at the root; asking there first
    // would find it instead of the one describing this endpoint.
    expect_eq(
        "the first resource metadata candidate",
        candidates.first().map(String::as_str),
        Some("https://mcp.example.com/.well-known/oauth-protected-resource/mcp"),
    )?;
    expect_eq(
        "the fallback candidate",
        candidates.get(1).map(String::as_str),
        Some("https://mcp.example.com/.well-known/oauth-protected-resource"),
    )
}

fn as_discovery_order() -> Verdict {
    // RFC 8414 inserts the well-known segment, OpenID Connect appends it, and
    // real deployments do both — so all three are tried, in this order.
    let with_path = server_metadata_candidates("https://auth.example.com/tenant1")
        .map_err(|e| e.to_string())?;
    expect_eq(
        "candidates for an issuer with a path",
        with_path,
        vec![
            "https://auth.example.com/.well-known/oauth-authorization-server/tenant1".to_string(),
            "https://auth.example.com/.well-known/openid-configuration/tenant1".to_string(),
            "https://auth.example.com/tenant1/.well-known/openid-configuration".to_string(),
        ],
    )?;

    let without_path =
        server_metadata_candidates("https://auth.example.com").map_err(|e| e.to_string())?;
    expect_eq(
        "candidates for a bare issuer",
        without_path,
        vec![
            "https://auth.example.com/.well-known/oauth-authorization-server".to_string(),
            "https://auth.example.com/.well-known/openid-configuration".to_string(),
        ],
    )
}

fn issuer_must_match() -> Verdict {
    // The check that stops a document at one host redirecting the flow to
    // another: RFC 8414 §3.3.
    expect(
        discovery::issuer_matches("https://as.example", "https://as.example"),
        "a document naming the issuer it was fetched for was refused",
    )?;
    expect(
        !discovery::issuer_matches("https://honest.example", "https://attacker.example"),
        "a document naming a different issuer than its URL was accepted",
    )
}

fn resource_must_cover_server() -> Verdict {
    // Exactly the substitution the conformance suite probes for.
    expect(
        !resource_matches("https://evil.example.com/mcp", "http://localhost:8931/mcp"),
        "metadata describing another host was accepted as describing this server",
    )?;
    // A document at the root legitimately covers the endpoints beneath it.
    expect(
        resource_matches("https://mcp.example.com", "https://mcp.example.com/mcp"),
        "a root document was refused for an endpoint it covers",
    )?;
    // But coverage stops at a segment boundary.
    expect(
        !resource_matches(
            "https://mcp.example.com/mcp",
            "https://mcp.example.com/mcp-admin",
        ),
        "coverage leaked across a partial path segment",
    )
}

fn iss_validation_table() -> Verdict {
    let advertising = metadata(serde_json::json!({
        "authorization_response_iss_parameter_supported": true
    }));
    let silent = metadata(serde_json::json!({}));

    // Advertised + present + matching: accepted.
    expect(
        validate_iss(&advertising, Some("https://as.example")).is_ok(),
        "a matching iss was rejected",
    )?;
    // Advertised + absent: rejected — the server promised one.
    expect(
        validate_iss(&advertising, None).is_err(),
        "a missing iss was accepted from a server that advertises sending one",
    )?;
    // Not advertised + present: compared anyway (the local-policy row).
    expect(
        validate_iss(&silent, Some("https://evil.example")).is_err(),
        "an unadvertised but wrong iss was accepted",
    )?;
    expect(
        validate_iss(&silent, Some("https://as.example")).is_ok(),
        "an unadvertised but correct iss was rejected",
    )?;
    // Neither: proceed.
    expect(
        validate_iss(&silent, None).is_ok(),
        "a response with no iss was rejected by a server that never promised one",
    )
}

fn iss_is_not_normalised() -> Verdict {
    let advertising = metadata(serde_json::json!({
        "authorization_response_iss_parameter_supported": true
    }));

    // Each of these would compare equal under a normalising comparison, and
    // each is a different issuer.
    for near_miss in [
        "https://as.example/",
        "HTTPS://AS.EXAMPLE",
        "https://as.example:443",
    ] {
        expect(
            validate_iss(&advertising, Some(near_miss)).is_err(),
            format!("{near_miss:?} was accepted for \"https://as.example\" after normalisation"),
        )?;
    }
    Ok(())
}

fn step_up_unions_scopes() -> Verdict {
    let held = vec!["files:read".to_string()];
    let challenged = vec!["files:write".to_string()];

    // A challenge states what *this* operation needs, not everything already
    // granted — so re-authorizing with the challenge alone would lose the rest.
    expect_eq(
        "the scopes a step-up requests",
        union_scopes(&held, &challenged),
        vec!["files:read".to_string(), "files:write".to_string()],
    )?;
    expect_eq(
        "a union that repeats a scope",
        union_scopes(&held, &held),
        held,
    )
}

fn pkce_is_s256() -> Verdict {
    // The RFC 7636 Appendix B vector: if this holds, the hash, the encoding
    // and the padding are all right.
    let known = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
    expect_eq(
        "the S256 challenge for the RFC 7636 verifier",
        known.challenge.as_str(),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
    )?;

    let generated = Pkce::generate();
    expect(
        (43..=128).contains(&generated.verifier.len()),
        format!(
            "a generated verifier is {} characters, outside RFC 7636's 43..128",
            generated.verifier.len()
        ),
    )?;
    expect(
        Pkce::generate().verifier != generated.verifier,
        "two generated verifiers were identical, so one could be replayed",
    )
}

fn challenge_parsing() -> Verdict {
    let parsed = challenge::parse(
        r#"Bearer resource_metadata="https://mcp.example.com/.well-known/oauth-protected-resource", scope="files:read files:write", error="insufficient_scope", error_description="needs write, then read""#,
    )
    .ok_or("a Bearer challenge was not recognised")?;

    expect_eq(
        "resource_metadata",
        parsed.resource_metadata.as_deref(),
        Some("https://mcp.example.com/.well-known/oauth-protected-resource"),
    )?;
    expect_eq(
        "the challenged scopes",
        parsed.scopes(),
        vec!["files:read".to_string(), "files:write".to_string()],
    )?;
    expect(
        parsed.is_insufficient_scope(),
        "an insufficient_scope challenge was not recognised as one",
    )?;
    // The comma inside the quoted description must not have split it.
    expect_eq(
        "error_description",
        parsed.error_description.as_deref(),
        Some("needs write, then read"),
    )?;
    // A scheme this client cannot answer is not a Bearer challenge.
    expect(
        challenge::parse("Basic realm=\"x\"").is_none(),
        "a Basic challenge was mistaken for one an OAuth flow could satisfy",
    )
}
