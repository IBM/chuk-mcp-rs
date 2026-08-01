//! Which `Host` and `Origin` values this server will answer.
//!
//! A server bound to loopback without TLS or authentication is reachable from
//! any web page the user happens to visit: the attacker points `evil.com` at
//! `127.0.0.1`, the browser dutifully connects, and without this check the
//! server treats the page as a local client. Validating the header the browser
//! is forced to send honestly is what closes it.
//!
//! See the MCP advisory
//! [GHSA-w48q-cv73-mx4w](https://github.com/modelcontextprotocol/typescript-sdk/security/advisories/GHSA-w48q-cv73-mx4w).

use std::net::IpAddr;

use hyper::header::{HeaderMap, HOST, ORIGIN};

/// The host name that means "this machine" but is not an address.
const LOCALHOST: &str = "localhost";

/// Which hosts a server answers to.
///
/// The default is loopback only, which is right for the case the advisory is
/// about — a local server with no other defence. A server that is reachable
/// from elsewhere is expected to say so.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum AllowedHosts {
    /// `localhost`, `127.0.0.0/8` and `::1`, with any port.
    #[default]
    Loopback,
    /// Loopback, plus the host names given. Ports are ignored: a name is
    /// allowed or it is not.
    Also(Vec<String>),
    /// Answer regardless of `Host` and `Origin`.
    ///
    /// Only correct when something else already establishes who is calling —
    /// TLS with authentication, or a network the server is not reachable from.
    /// On a plain local server this re-opens the rebinding hole.
    Any,
}

impl AllowedHosts {
    /// Whether a host name — no port, no brackets — may be answered.
    fn permits(&self, host: &str) -> bool {
        match self {
            AllowedHosts::Any => true,
            AllowedHosts::Loopback => is_loopback(host),
            AllowedHosts::Also(names) => {
                is_loopback(host) || names.iter().any(|name| name.eq_ignore_ascii_case(host))
            }
        }
    }
}

/// Whether a host names this machine.
///
/// `is_loopback` rather than a literal comparison, so the whole of
/// `127.0.0.0/8` counts: `127.0.0.2` reaches the same server as `127.0.0.1`,
/// and an allowlist that missed it would be a false sense of a closed door.
fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case(LOCALHOST)
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

/// The host part of an authority, without its port or IPv6 brackets.
///
/// `[::1]:8080` and `127.0.0.1:8080` both have to reduce to something
/// [`is_loopback`] can parse.
fn host_of(authority: &str) -> &str {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    // An unbracketed address with several colons is a bare IPv6 literal —
    // irregular in a Host header, but splitting it on ':' would leave nothing
    // recognisable, and "unparseable" must not read as "allowed".
    if authority.matches(':').count() > 1 {
        return authority;
    }
    authority.split(':').next().unwrap_or(authority)
}

/// The host an `Origin` names, or `None` if it names nothing — a serialised
/// opaque origin (`null`), which no allowlist should match.
fn origin_host(origin: &str) -> Option<&str> {
    let origin = origin.trim();
    if origin.eq_ignore_ascii_case("null") || origin.is_empty() {
        return None;
    }
    let authority = origin
        .split_once("://")
        .map(|(_scheme, rest)| rest)
        .unwrap_or(origin);
    // Anything after the authority is not ours to interpret.
    let authority = authority.split(['/', '?', '#']).next().unwrap_or(authority);
    Some(host_of(authority))
}

/// Whether this request may be answered, or the reason it may not.
///
/// Both headers are checked when both are sent. `Origin` is the one a browser
/// sets and cannot be talked out of, so a request carrying a hostile `Origin`
/// is refused even where its `Host` looks fine.
pub fn check(headers: &HeaderMap, allowed: &AllowedHosts) -> Result<(), String> {
    if let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) {
        match origin_host(origin) {
            Some(host) if allowed.permits(host) => {}
            _ => return Err(format!("Origin {origin} is not allowed")),
        }
    }

    if let Some(host) = headers.get(HOST).and_then(|value| value.to_str().ok()) {
        if !allowed.permits(host_of(host)) {
            return Err(format!("Host {host} is not allowed"));
        }
    }

    // Neither header present leaves nothing to validate. HTTP/1.1 requires
    // `Host`, so this is the HTTP/1.0 client and the direct socket — neither of
    // which is a browser, and the attack needs a browser.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                hyper::header::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
                value.parse().expect("a header value"),
            );
        }
        headers
    }

    #[test]
    fn an_authority_reduces_to_its_host() {
        assert_eq!(host_of("127.0.0.1:8080"), "127.0.0.1");
        assert_eq!(host_of("localhost"), "localhost");
        assert_eq!(host_of("[::1]:8080"), "::1");
        assert_eq!(host_of("[::1]"), "::1");
        // A bare IPv6 literal keeps its colons rather than reducing to "".
        assert_eq!(host_of("::1"), "::1");
    }

    #[test]
    fn an_origin_reduces_to_its_host() {
        assert_eq!(origin_host("http://localhost:3000"), Some("localhost"));
        assert_eq!(origin_host("https://evil.com"), Some("evil.com"));
        assert_eq!(origin_host("http://[::1]:9000/path"), Some("::1"));
        // An opaque origin matches nothing, rather than matching everything.
        assert_eq!(origin_host("null"), None);
        assert_eq!(origin_host(""), None);
    }

    #[test]
    fn loopback_is_the_whole_127_block_and_the_v6_address() {
        assert!(is_loopback("localhost"));
        assert!(is_loopback("LocalHost"));
        assert!(is_loopback("127.0.0.1"));
        assert!(is_loopback("127.0.0.2"));
        assert!(is_loopback("::1"));
        assert!(!is_loopback("evil.com"));
        assert!(!is_loopback("10.0.0.1"));
        // The prefix of a loopback name is not a loopback name.
        assert!(!is_loopback("localhost.evil.com"));
    }

    #[test]
    fn the_default_answers_loopback_and_refuses_the_rest() {
        let allowed = AllowedHosts::default();
        assert!(check(&headers(&[("host", "127.0.0.1:8931")]), &allowed).is_ok());
        assert!(check(&headers(&[("host", "localhost:8931")]), &allowed).is_ok());
        assert!(check(&headers(&[("host", "[::1]:8931")]), &allowed).is_ok());

        let refused = check(&headers(&[("host", "evil.com")]), &allowed);
        assert!(refused.is_err());
        assert!(refused.unwrap_err().contains("evil.com"));
    }

    #[test]
    fn a_hostile_origin_is_refused_even_with_an_innocent_host() {
        // The rebinding case exactly: the browser is talking to 127.0.0.1 and
        // says so, but the page asking is not one this server serves.
        let refused = check(
            &headers(&[("host", "127.0.0.1:8931"), ("origin", "http://evil.com")]),
            &AllowedHosts::default(),
        );
        assert!(refused.is_err());
        assert!(refused.unwrap_err().contains("evil.com"));
    }

    #[test]
    fn a_named_host_is_answered_only_once_it_is_allowed() {
        let headers = headers(&[
            ("host", "mcp.example.com"),
            ("origin", "https://app.example.com"),
        ]);

        assert!(check(&headers, &AllowedHosts::default()).is_err());
        assert!(check(
            &headers,
            &AllowedHosts::Also(vec!["mcp.example.com".into(), "app.example.com".into()])
        )
        .is_ok());
        assert!(check(&headers, &AllowedHosts::Any).is_ok());
    }

    #[test]
    fn a_request_with_neither_header_has_nothing_to_validate() {
        assert!(check(&headers(&[]), &AllowedHosts::default()).is_ok());
    }
}
