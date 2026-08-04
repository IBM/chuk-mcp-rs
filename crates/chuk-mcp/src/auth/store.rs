//! Where tokens and client registrations live between requests.
//!
//! Everything here is keyed by **issuer** as well as resource, because the
//! specification requires it: client identifiers are unique to the
//! authorization server that issued them, and a client that reused a
//! registration across servers would be presenting somebody else's identity.
//! When the protected resource metadata starts naming a different issuer, the
//! old entry simply does not match and the client registers again — which is
//! the required behaviour falling out of the key rather than being remembered
//! as a special case.
//!
//! Nothing is written to disk. This crate never persists secrets on a caller's
//! behalf: where a refresh token may safely be stored is a decision about the
//! surrounding system — a keychain, an encrypted file, nowhere at all — and it
//! is not one a protocol library can make. Implement [`TokenStore`] to make
//! that choice explicitly.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// What an authorization server issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tokens {
    pub access_token: String,
    /// Present only when the server chose to issue one — the specification is
    /// explicit that a client **MUST NOT** assume it will.
    pub refresh_token: Option<String>,
    /// The scopes actually granted, which may be narrower than those asked for.
    pub scopes: Vec<String>,
    /// When the access token stops being usable, if the server said.
    pub expires_at: Option<Instant>,
}

impl Tokens {
    /// Whether the access token can still be used.
    ///
    /// A token with no stated lifetime is treated as live: the server chose
    /// not to say, and guessing an expiry would throw away a working token.
    pub fn is_live(&self) -> bool {
        match self.expires_at {
            Some(at) => Instant::now() < at,
            None => true,
        }
    }

    /// Whether it is close enough to expiry to be worth refreshing now.
    ///
    /// The margin exists because a token that expires in flight fails the
    /// request it was attached to, and a retry costs more than refreshing a
    /// few seconds early.
    pub fn expires_within(&self, margin: Duration) -> bool {
        match self.expires_at {
            Some(at) => Instant::now() + margin >= at,
            None => false,
        }
    }
}

/// A client identity at one authorization server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    pub client_id: String,
    /// Present for a confidential client; absent for a public one and for
    /// Client ID Metadata Documents, which carry no secret by construction.
    pub client_secret: Option<String>,
}

/// Which authorization server, and for which resource.
///
/// Both halves matter. The issuer is what makes a registration or token
/// non-portable; the resource is what an access token's audience is bound to,
/// so one server's token is not reused against another.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Key {
    pub issuer: String,
    pub resource: String,
}

impl Key {
    pub fn new(issuer: impl Into<String>, resource: impl Into<String>) -> Self {
        Key {
            issuer: issuer.into(),
            resource: resource.into(),
        }
    }
}

/// Somewhere to keep tokens and registrations.
///
/// Implement this to persist across runs. The default
/// [`InMemoryTokenStore`] keeps everything for the life of the process and
/// nowhere else.
pub trait TokenStore: Send + Sync {
    fn tokens(&self, key: &Key) -> Option<Tokens>;
    fn put_tokens(&self, key: Key, tokens: Tokens);
    /// Forget a token that turned out not to work, so the next request
    /// re-authorizes rather than replaying a rejection.
    fn clear_tokens(&self, key: &Key);

    /// A registration is keyed by issuer alone: the same client identity is
    /// used for every resource that authorization server protects.
    fn registration(&self, issuer: &str) -> Option<Registration>;
    fn put_registration(&self, issuer: String, registration: Registration);
}

/// The default store: everything in memory, nothing on disk.
#[derive(Default)]
pub struct InMemoryTokenStore {
    tokens: Mutex<BTreeMap<Key, Tokens>>,
    registrations: Mutex<BTreeMap<String, Registration>>,
}

impl InMemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A shared handle, which is how a store is normally passed around.
    pub fn shared() -> Arc<dyn TokenStore> {
        Arc::new(Self::new())
    }
}

impl TokenStore for InMemoryTokenStore {
    fn tokens(&self, key: &Key) -> Option<Tokens> {
        self.tokens.lock().expect("token lock").get(key).cloned()
    }

    fn put_tokens(&self, key: Key, tokens: Tokens) {
        self.tokens.lock().expect("token lock").insert(key, tokens);
    }

    fn clear_tokens(&self, key: &Key) {
        self.tokens.lock().expect("token lock").remove(key);
    }

    fn registration(&self, issuer: &str) -> Option<Registration> {
        self.registrations
            .lock()
            .expect("registration lock")
            .get(issuer)
            .cloned()
    }

    fn put_registration(&self, issuer: String, registration: Registration) {
        self.registrations
            .lock()
            .expect("registration lock")
            .insert(issuer, registration);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(access: &str) -> Tokens {
        Tokens {
            access_token: access.to_string(),
            refresh_token: None,
            scopes: vec![],
            expires_at: None,
        }
    }

    #[test]
    fn a_token_round_trips_through_the_store() {
        let store = InMemoryTokenStore::new();
        let key = Key::new("https://as.example", "https://mcp.example/mcp");

        assert!(store.tokens(&key).is_none());
        store.put_tokens(key.clone(), tokens("abc"));
        assert_eq!(store.tokens(&key).unwrap().access_token, "abc");

        store.clear_tokens(&key);
        assert!(store.tokens(&key).is_none());
    }

    /// The requirement that makes authorization-server migration work: a token
    /// minted by one issuer is simply not found when another is in play.
    #[test]
    fn tokens_do_not_leak_across_issuers() {
        let store = InMemoryTokenStore::new();
        let resource = "https://mcp.example/mcp";
        store.put_tokens(Key::new("https://one.example", resource), tokens("one"));

        assert!(store
            .tokens(&Key::new("https://two.example", resource))
            .is_none());
        assert_eq!(
            store
                .tokens(&Key::new("https://one.example", resource))
                .unwrap()
                .access_token,
            "one"
        );
    }

    /// An access token is bound to an audience, so one resource's token must
    /// not be presented to another.
    #[test]
    fn tokens_do_not_leak_across_resources() {
        let store = InMemoryTokenStore::new();
        let issuer = "https://as.example";
        store.put_tokens(Key::new(issuer, "https://a.example/mcp"), tokens("a"));

        assert!(store
            .tokens(&Key::new(issuer, "https://b.example/mcp"))
            .is_none());
    }

    /// A registration belongs to an authorization server, not to a resource.
    #[test]
    fn a_registration_is_found_for_any_resource_at_its_issuer() {
        let store = InMemoryTokenStore::new();
        store.put_registration(
            "https://as.example".to_string(),
            Registration {
                client_id: "client-1".into(),
                client_secret: None,
            },
        );

        assert_eq!(
            store.registration("https://as.example").unwrap().client_id,
            "client-1"
        );
        assert!(store.registration("https://other.example").is_none());
    }

    #[test]
    fn a_token_without_an_expiry_is_live() {
        assert!(tokens("x").is_live());
        assert!(!tokens("x").expires_within(Duration::from_secs(60)));
    }

    #[test]
    fn an_expired_token_is_not_live() {
        let expired = Tokens {
            expires_at: Some(Instant::now() - Duration::from_secs(1)),
            ..tokens("x")
        };
        assert!(!expired.is_live());
        assert!(expired.expires_within(Duration::from_secs(0)));
    }

    /// A token about to expire is refreshed early rather than sent and lost.
    #[test]
    fn a_token_near_expiry_is_worth_refreshing() {
        let nearly = Tokens {
            expires_at: Some(Instant::now() + Duration::from_secs(5)),
            ..tokens("x")
        };
        assert!(nearly.is_live());
        assert!(nearly.expires_within(Duration::from_secs(30)));
        assert!(!nearly.expires_within(Duration::from_secs(1)));
    }
}
