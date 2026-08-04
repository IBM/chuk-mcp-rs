//! The authorization state one connection carries.
//!
//! [`Auth`] is configuration — the same for every connection made with it.
//! This is what a *particular* connection accumulates: the token it currently
//! holds, the scopes that token was granted for, and how many times it has
//! already been sent back for more.
//!
//! Keeping the policy here rather than in the transports means each transport
//! needs one hook — "here is a `401`, what now?" — instead of its own copy of
//! the scope union, the retry budget and the difference between a token that
//! is missing and one that is merely too narrow.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use crate::protocol::types::errors::McpError;

use super::challenge::{self, Challenge};
use super::flow;
use super::Auth;

/// What a challenge should make the caller do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A token was obtained or replaced. Send the request again.
    Retry,
    /// Nothing more can be done; report the response as it stands.
    ///
    /// Returned rather than an error because a `403` a client cannot widen is
    /// still a perfectly good answer for the caller to see.
    GiveUp,
}

/// The authorization state of one connection.
pub struct AuthSession {
    auth: Auth,
    client: reqwest::Client,
    /// The MCP endpoint this session authorizes against.
    server_url: String,
    /// The bearer token in use, and the scopes it carries.
    held: Mutex<Option<Held>>,
    /// How many times this session has re-authorized for wider scope.
    upgrades: AtomicUsize,
}

#[derive(Clone)]
struct Held {
    access_token: String,
    scopes: Vec<String>,
}

impl AuthSession {
    pub fn new(auth: Auth, server_url: impl Into<String>) -> Self {
        AuthSession {
            auth,
            client: reqwest::Client::new(),
            server_url: server_url.into(),
            held: Mutex::new(None),
            upgrades: AtomicUsize::new(0),
        }
    }

    /// The token to put on the next request, if this session has one.
    pub fn bearer(&self) -> Option<String> {
        self.held
            .lock()
            .expect("auth session lock")
            .as_ref()
            .map(|held| held.access_token.clone())
    }

    /// Decide what to do about an authorization failure.
    ///
    /// `status` and the raw `WWW-Authenticate` value are all a transport needs
    /// to hand over. A response that is not an authorization failure, or that
    /// carries no challenge this client can answer, is [`Outcome::GiveUp`] —
    /// which leaves the transport reporting the original response, as it
    /// should.
    pub async fn handle_response(
        &self,
        status: u16,
        www_authenticate: Option<&str>,
    ) -> Result<Outcome, McpError> {
        if !matches!(status, 401 | 403) {
            return Ok(Outcome::GiveUp);
        }
        // A 401 with no challenge at all is still worth one attempt: the
        // server said "unauthorized", and the well-known locations may yet
        // name an authorization server. A 403 without one is not — nothing
        // says what wider scope would even be.
        let challenge = match www_authenticate.and_then(challenge::parse) {
            Some(challenge) => challenge,
            None if status == 401 => Challenge::default(),
            None => return Ok(Outcome::GiveUp),
        };

        if status == 403 && !challenge.is_insufficient_scope() {
            // Forbidden for a reason more scope cannot fix.
            return Ok(Outcome::GiveUp);
        }

        // Widening scope is bounded. A server that keeps challenging would
        // otherwise loop a user through consent screens indefinitely.
        if status == 403 || self.bearer().is_some() {
            let used = self.upgrades.fetch_add(1, Ordering::SeqCst);
            if used >= self.auth.max_scope_upgrades {
                tracing::warn!(
                    "giving up after {used} authorization attempt(s) for {}",
                    self.server_url
                );
                return Ok(Outcome::GiveUp);
            }
        }

        // The union of what is already held and what is being asked for. A
        // challenge names what *this* operation needs, so re-authorizing with
        // only that would drop permissions granted for everything else.
        let held = self.held.lock().expect("auth session lock").clone();
        let wanted = flow::union_scopes(
            &held.map(|held| held.scopes).unwrap_or_default(),
            &challenge.scopes(),
        );

        let authorized = flow::authorize(
            &self.client,
            &self.auth,
            &self.server_url,
            &challenge,
            &wanted,
        )
        .await?;

        *self.held.lock().expect("auth session lock") = Some(Held {
            access_token: authorized.tokens.access_token.clone(),
            scopes: authorized.tokens.scopes.clone(),
        });
        Ok(Outcome::Retry)
    }

    // Deliberately no `prime()`. Reusing a stored token before discovery has
    // run would mean guessing which authorization server this connection
    // belongs to, and the store is keyed by issuer precisely because that
    // cannot be guessed: a resource whose authorization server has changed
    // would be sent a token the new one never issued, which the specification
    // forbids outright. The `401` that discovery is driven from costs one
    // round trip and is always right.
}

impl std::fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthSession")
            .field("server_url", &self.server_url)
            .field("has_token", &self.bearer().is_some())
            .field("upgrades", &self.upgrades.load(Ordering::SeqCst))
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> AuthSession {
        AuthSession::new(Auth::new(), "https://mcp.example.com/mcp")
    }

    #[tokio::test]
    async fn a_successful_status_is_not_an_authorization_problem() {
        let session = session();
        for status in [200, 202, 400, 404, 500] {
            assert_eq!(
                session.handle_response(status, None).await.unwrap(),
                Outcome::GiveUp
            );
        }
    }

    /// Forbidden for a reason more scope cannot fix is not re-authorized.
    #[tokio::test]
    async fn a_403_without_an_insufficient_scope_challenge_gives_up() {
        let session = session();
        assert_eq!(
            session
                .handle_response(403, Some(r#"Bearer error="invalid_token""#))
                .await
                .unwrap(),
            Outcome::GiveUp
        );
        assert_eq!(
            session.handle_response(403, None).await.unwrap(),
            Outcome::GiveUp
        );
    }

    /// The budget stops a server that challenges forever.
    #[tokio::test]
    async fn repeated_scope_challenges_are_bounded() {
        let session = AuthSession::new(
            Auth::new().max_scope_upgrades(0),
            "https://mcp.example.com/mcp",
        );
        let outcome = session
            .handle_response(403, Some(r#"Bearer error="insufficient_scope", scope="a""#))
            .await
            .unwrap();
        assert_eq!(outcome, Outcome::GiveUp);
    }

    #[test]
    fn a_fresh_session_holds_no_token() {
        assert!(session().bearer().is_none());
    }

    /// The debug view is for diagnosing a flow, not for reading the token out
    /// of a log.
    #[test]
    fn the_debug_view_does_not_print_the_token() {
        let rendered = format!("{:?}", session());
        assert!(rendered.contains("has_token: false"));
        assert!(!rendered.contains("access_token"));
    }
}
