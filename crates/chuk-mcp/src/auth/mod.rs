//! OAuth 2.1 authorization for HTTP transports.
//!
//! Authorization is optional in MCP and only meaningful over HTTP — a stdio
//! server is a subprocess, and the specification says such clients should take
//! credentials from the environment instead.
//!
//! # What a caller supplies
//!
//! The library performs discovery, registration, PKCE, the token exchange and
//! refresh. It cannot perform the one step that needs a human: sending the user
//! to the authorization server and getting them back. That is
//! [`AuthorizationHandler`] — given a URL, put it in front of the user however
//! this application does that. The redirect itself is handled here: a loopback
//! listener is bound before the handler is called and the callback is read off
//! it.
//!
//! ```no_run
//! use std::sync::Arc;
//! use chuk_mcp::auth::{Auth, AuthorizationHandler};
//! use chuk_mcp::McpError;
//!
//! struct PrintTheUrl;
//!
//! #[async_trait::async_trait]
//! impl AuthorizationHandler for PrintTheUrl {
//!     async fn authorize(&self, url: &str) -> Result<(), McpError> {
//!         println!("open this to authorize: {url}");
//!         Ok(())
//!     }
//! }
//!
//! # async fn run() -> Result<(), McpError> {
//! let client = chuk_mcp::connect::Connect::to("https://mcp.example.com/mcp")
//!     .authorization(Auth::new().handler(Arc::new(PrintTheUrl)))
//!     .connect()
//!     .await?;
//! # Ok(()) }
//! ```
//!
//! # What is not here
//!
//! The authorization *extensions* — DPoP, client credentials, JWT bearer — are
//! separate optional specifications and are not implemented. Neither is
//! `private_key_jwt` client authentication.

pub mod challenge;
pub mod discovery;
pub mod flow;
pub mod pkce;
pub mod registration;
pub mod session;
pub mod store;

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

pub use challenge::Challenge;
pub use discovery::{AuthorizationServerMetadata, ProtectedResourceMetadata};
pub use registration::ClientIdentity;
pub use session::{AuthSession, Outcome};
pub use store::{InMemoryTokenStore, Key, Registration, TokenStore, Tokens};

use crate::protocol::types::errors::McpError;

/// Put an authorization URL in front of the user.
///
/// The implementation decides how — opening a browser, printing the URL,
/// showing it in a UI. It returns as soon as the user has been sent; the
/// library is already listening for the redirect and does the waiting.
///
/// Returning an error aborts the flow, which is how a handler declines: a
/// headless process with no way to reach a user should say so rather than
/// leave the connection hanging until the callback times out.
#[async_trait]
pub trait AuthorizationHandler: Send + Sync {
    async fn authorize(&self, url: &str) -> Result<(), McpError>;
}

/// The handler a caller gets by not choosing one: it refuses.
///
/// Deliberately inert rather than convenient. The alternative default —
/// fetching the authorization URL from this process — would make every
/// unconfigured client issue an unattended request to an address the *server*
/// chose, since the authorization endpoint is named by metadata discovered
/// from it. That is a request an application should opt into knowingly, not
/// inherit.
pub struct RefuseToAuthorize;

#[async_trait]
impl AuthorizationHandler for RefuseToAuthorize {
    async fn authorize(&self, _url: &str) -> Result<(), McpError> {
        Err(McpError::validation(
            "this server requires authorization, but no AuthorizationHandler was \
             supplied: use Auth::handler to say how the user should be sent to the \
             authorization server, or auth::FollowRedirect for an unattended client"
                .to_string(),
        ))
    }
}

/// An [`AuthorizationHandler`] that fetches the URL from this process.
///
/// For a server that needs no human interaction — a test double, a
/// pre-consented service — and for the conformance suite, whose authorization
/// endpoint redirects straight back. It is **not** a way to log a user in:
/// against a real authorization server this lands on a login page and the
/// callback never arrives.
///
/// # What you are opting into
///
/// The URL is built from the authorization endpoint in the authorization
/// server's metadata, which was found by following the issuer the *MCP server*
/// named. Using this handler therefore lets a server it talks to direct an
/// outbound request from this process to an address of that server's choosing
/// — an internal host, a cloud metadata endpoint. A browser-based handler does
/// not have this property, because the request comes from the browser and is
/// visible to the user. Prefer one wherever there is a user.
pub struct FollowRedirect {
    client: reqwest::Client,
}

impl FollowRedirect {
    pub fn new() -> Self {
        FollowRedirect {
            // The redirect is followed so the callback reaches the listener
            // this crate opened, exactly as a browser would deliver it.
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::limited(10))
                .build()
                .unwrap_or_default(),
        }
    }
}

impl Default for FollowRedirect {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AuthorizationHandler for FollowRedirect {
    async fn authorize(&self, url: &str) -> Result<(), McpError> {
        // The response is deliberately ignored: what matters is that the
        // redirect chain ended at the loopback listener, which is reading it.
        let _ = self.client.get(url).send().await;
        Ok(())
    }
}

/// How long to wait for the user to come back before giving up.
const DEFAULT_CALLBACK_TIMEOUT: Duration = Duration::from_secs(300);

/// How many times to re-authorize for more scope before treating it as a
/// permanent failure.
///
/// The specification asks for "no more than a few". Two is enough for the
/// legitimate case — one step-up, one retry — and small enough that a server
/// challenging in a loop is stopped rather than followed.
const DEFAULT_MAX_SCOPE_UPGRADES: usize = 2;

/// Authorization configuration for a connection.
#[derive(Clone)]
pub struct Auth {
    /// How the user is sent to the authorization server.
    pub(crate) handler: Arc<dyn AuthorizationHandler>,
    /// Where tokens and registrations are kept.
    pub(crate) store: Arc<dyn TokenStore>,
    /// Who this client says it is when registering.
    pub(crate) identity: ClientIdentity,
    /// How long to wait for the redirect.
    pub(crate) callback_timeout: Duration,
    /// How many scope upgrades to attempt before giving up.
    pub(crate) max_scope_upgrades: usize,
    /// Whether to ask for a refresh token when the server offers the scope.
    pub(crate) want_refresh_token: bool,
}

impl Auth {
    /// Authorization with the defaults: tokens in memory, a client that
    /// registers dynamically if it must, and **no** way to reach a user.
    ///
    /// [`Auth::handler`] is not optional in practice — without one the first
    /// challenge fails with an error saying so. That is deliberate: see
    /// [`RefuseToAuthorize`] for why the convenient default would be the wrong
    /// one.
    pub fn new() -> Self {
        Auth {
            handler: Arc::new(RefuseToAuthorize),
            store: InMemoryTokenStore::shared(),
            identity: ClientIdentity::default(),
            callback_timeout: DEFAULT_CALLBACK_TIMEOUT,
            max_scope_upgrades: DEFAULT_MAX_SCOPE_UPGRADES,
            want_refresh_token: true,
        }
    }

    /// How the user is sent to the authorization server.
    pub fn handler(mut self, handler: Arc<dyn AuthorizationHandler>) -> Self {
        self.handler = handler;
        self
    }

    /// Where tokens and registrations are kept between requests.
    pub fn store(mut self, store: Arc<dyn TokenStore>) -> Self {
        self.store = store;
        self
    }

    /// Who this client says it is.
    pub fn identity(mut self, identity: ClientIdentity) -> Self {
        self.identity = identity;
        self
    }

    /// How long to wait for the redirect to come back.
    pub fn callback_timeout(mut self, timeout: Duration) -> Self {
        self.callback_timeout = timeout;
        self
    }

    /// How many times to re-authorize for more scope before giving up.
    pub fn max_scope_upgrades(mut self, attempts: usize) -> Self {
        self.max_scope_upgrades = attempts;
        self
    }

    /// Whether to ask for a refresh token when the authorization server offers
    /// `offline_access`.
    ///
    /// On by default. A client that would rather re-authorize than hold a
    /// long-lived credential turns it off.
    pub fn want_refresh_token(mut self, want: bool) -> Self {
        self.want_refresh_token = want;
        self
    }
}

impl Default for Auth {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Auth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Auth")
            .field("identity", &self.identity)
            .field("callback_timeout", &self.callback_timeout)
            .field("max_scope_upgrades", &self.max_scope_upgrades)
            .field("want_refresh_token", &self.want_refresh_token)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_documented_ones() {
        let auth = Auth::new();
        assert_eq!(auth.callback_timeout, DEFAULT_CALLBACK_TIMEOUT);
        assert_eq!(auth.max_scope_upgrades, DEFAULT_MAX_SCOPE_UPGRADES);
        assert!(auth.want_refresh_token);
    }

    #[test]
    fn the_builder_replaces_what_it_is_given() {
        let auth = Auth::new()
            .callback_timeout(Duration::from_secs(1))
            .max_scope_upgrades(5)
            .want_refresh_token(false);

        assert_eq!(auth.callback_timeout, Duration::from_secs(1));
        assert_eq!(auth.max_scope_upgrades, 5);
        assert!(!auth.want_refresh_token);
    }

    /// The debug view must not become a way to print a token store's contents.
    #[test]
    fn the_debug_view_shows_configuration_not_secrets() {
        let rendered = format!("{:?}", Auth::new());
        assert!(rendered.contains("callback_timeout"));
        assert!(!rendered.to_lowercase().contains("token_store"));
    }
}
