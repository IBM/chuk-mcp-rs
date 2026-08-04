//! The [`Connect`] builder — everything [`connect`](super::connect) does, with
//! the knobs exposed.

use std::collections::HashMap;
use std::time::Duration;

use std::sync::Arc;

use crate::client::input::InputHandler;
use crate::client::McpClient;
use crate::protocol::envelope::ClientIdentity;
use crate::protocol::era::EraMode;
use crate::protocol::types::capabilities::ElicitationCapability;
use crate::protocol::types::errors::McpError;
use crate::transports::http_dual::{DualEraHttpParameters, DualEraHttpTransport};
use crate::transports::limits::TransportLimits;
use crate::transports::stdio::StdioParameters;
use crate::transports::stdio_dual::{stdio_client_dual, StdioDualOptions};
use crate::transports::Transport;

use super::settle::settle;
use super::target::Target;

/// How to connect, once [`Target`] has said where.
///
/// Split from the target so that `connect` can take the two apart without
/// either half having to be cloned or rebuilt.
struct Options {
    mode: EraMode,
    identity: ClientIdentity,
    timeout: Option<Duration>,
    limits: TransportLimits,
    bearer_token: Option<String>,
    headers: HashMap<String, String>,
    env: Option<HashMap<String, String>>,
    credential_context: Option<String>,
    input_handler: Option<Arc<dyn InputHandler>>,
    /// OAuth configuration, when the caller supplied any.
    #[cfg(feature = "auth")]
    auth: Option<crate::auth::Auth>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            mode: EraMode::default(),
            identity: ClientIdentity::chuk(),
            #[cfg(feature = "auth")]
            auth: None,
            timeout: None,
            limits: TransportLimits::default(),
            bearer_token: None,
            headers: HashMap::new(),
            env: None,
            credential_context: None,
            input_handler: None,
        }
    }
}

/// A connection being described.
///
/// ```no_run
/// use chuk_mcp::Connect;
/// # async fn run() -> Result<(), chuk_mcp::McpError> {
/// let client = Connect::to("https://example.com/mcp")
///     .bearer_token("…")
///     .header("X-Tenant", "acme")
///     .connect()
///     .await?;
/// # Ok(()) }
/// ```
///
/// Options that do not apply to the target are ignored: `bearer_token` and
/// `header` mean nothing to a subprocess, `env` means nothing to an endpoint.
/// Every builder method is infallible; a malformed target surfaces when
/// [`Connect::connect`] runs.
pub struct Connect {
    target: Result<Target, McpError>,
    options: Options,
}

impl Connect {
    /// Describe a connection to a URL or a command line. See [`Target::parse`]
    /// for how the string is read.
    pub fn to(target: impl AsRef<str>) -> Connect {
        Connect::with_target(Target::parse(target.as_ref()))
    }

    /// Describe a connection to a subprocess, with the command and arguments
    /// given explicitly — for command lines whitespace splitting would mangle.
    pub fn to_command<I, S>(command: impl Into<String>, args: I) -> Connect
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Connect::with_target(Ok(Target::command(command, args)))
    }

    /// Describe a connection to an HTTP endpoint.
    pub fn to_url(url: impl Into<String>) -> Connect {
        Connect::with_target(Ok(Target::url(url)))
    }

    fn with_target(target: Result<Target, McpError>) -> Connect {
        Connect {
            target,
            options: Options::default(),
        }
    }

    /// Pin the protocol era instead of detecting it.
    pub fn era(mut self, mode: EraMode) -> Connect {
        self.options.mode = mode;
        self
    }

    /// Who to say we are. Defaults to this library's identity.
    pub fn identity(mut self, identity: ClientIdentity) -> Connect {
        self.options.identity = identity;
        self
    }

    /// Timeout for the probe or handshake.
    pub fn timeout(mut self, timeout: Duration) -> Connect {
        self.options.timeout = Some(timeout);
        self
    }

    /// Framing limits for the transport.
    pub fn limits(mut self, limits: TransportLimits) -> Connect {
        self.options.limits = limits;
        self
    }

    /// Bearer token, for an HTTP target.
    pub fn bearer_token(mut self, token: impl Into<String>) -> Connect {
        self.options.bearer_token = Some(token.into());
        self
    }

    /// An extra HTTP header. Call repeatedly for more than one.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Connect {
        self.options.headers.insert(name.into(), value.into());
        self
    }

    /// Scope the era decision to a credential context — an opaque, stable
    /// identity for the credential in use, **never a raw token**. One endpoint
    /// can serve different eras to different principals.
    pub fn credential_context(mut self, context: impl Into<String>) -> Connect {
        self.options.credential_context = Some(context.into());
        self
    }

    /// Environment for a subprocess target. Defaults to
    /// [`get_default_environment`](crate::transports::stdio::get_default_environment).
    pub fn env(mut self, env: HashMap<String, String>) -> Connect {
        self.options.env = Some(env);
        self
    }

    /// Authorize against a protected server.
    ///
    /// Without this a `401` is reported as the transport failure it is. With
    /// it, the challenge is answered: metadata discovered, a client registered,
    /// the user sent to the authorization server, and the token attached to
    /// every request after.
    #[cfg(feature = "auth")]
    pub fn authorization(mut self, auth: crate::auth::Auth) -> Connect {
        self.options.auth = Some(auth);
        self
    }

    /// Answer servers that ask for user input.
    ///
    /// Also declares the matching `elicitation` capability, in the modern
    /// `_meta` and the legacy handshake alike — a server **MUST NOT** ask for
    /// input a client has not declared it can supply, so setting the handler
    /// and declaring the capability are one action rather than two things to
    /// remember to keep in step.
    pub fn input_handler(mut self, handler: Arc<dyn InputHandler>) -> Connect {
        self.options.identity.capabilities.elicitation = Some(ElicitationCapability::modes(
            true,
            handler.supports_url_mode(),
        ));
        self.options.input_handler = Some(handler);
        self
    }

    /// Open the connection, settle the era, and return a ready client.
    pub async fn connect(self) -> Result<McpClient, McpError> {
        let Connect { target, options } = self;
        match target? {
            Target::Stdio { command, args } => options.connect_stdio(command, args).await,
            Target::Http { url } => options.connect_http(url).await,
        }
    }
}

impl Options {
    async fn connect_stdio(
        self,
        command: String,
        args: Vec<String>,
    ) -> Result<McpClient, McpError> {
        let mut parameters = StdioParameters::new(command, args);
        parameters.env = self.env;

        let defaults = StdioDualOptions::default();
        let connection = stdio_client_dual(
            parameters,
            StdioDualOptions {
                mode: self.mode,
                identity: self.identity,
                limits: self.limits,
                timeout: self.timeout.or(defaults.timeout),
            },
        )
        .await?;

        let mut client = McpClient::from_profile(
            connection.transport,
            connection.read,
            connection.write,
            connection.profile,
        );
        if let Some(handler) = self.input_handler {
            client.set_input_handler(handler);
        }
        Ok(client)
    }

    async fn connect_http(self, url: String) -> Result<McpClient, McpError> {
        #[cfg(feature = "auth")]
        let url_for_auth = url.clone();
        let mut parameters = DualEraHttpParameters::new(url)?
            .with_mode(self.mode)
            .with_headers(self.headers);
        // The transport injects `_meta` on every modern request from its own
        // identity, so an identity set here has to reach it — otherwise the
        // capabilities a caller declared (elicitation, most of all) are built
        // into the probe and then overwritten on every request after it.
        parameters.identity = self.identity.clone();
        if let Some(token) = self.bearer_token {
            parameters = parameters.with_bearer_token(token);
        }
        if let Some(context) = self.credential_context {
            parameters = parameters.with_credential_context(context);
        }

        // Built here rather than in the transport: the session is per
        // endpoint, and the transport is the thing being pointed at it.
        #[cfg(feature = "auth")]
        if let Some(auth) = self.auth.clone() {
            parameters.auth = Some(std::sync::Arc::new(crate::auth::AuthSession::new(
                auth,
                &url_for_auth,
            )));
        }

        let transport = DualEraHttpTransport::start_with_limits(parameters, self.limits)?;
        let (read, write) = transport.get_streams().await?;

        // HTTP has no free probe — the first request *is* the probe — so it is
        // issued here, deliberately, rather than leaving the caller's first
        // call to discover the era by accident.
        let profile = settle(&read, &write, self.mode, &self.identity, self.timeout).await?;

        // Tell the transport what the handshake settled. Without this the two
        // can disagree — see `DualEraHttpTransport::set_era` — and a legacy
        // peer would be driven down the modern path for every request after
        // the handshake that proved it legacy.
        transport.set_era(profile.era);

        // A legacy peer answers server-initiated requests on a `GET` stream
        // that has to exist before the first call goes out. Opening it is a
        // round trip; waiting for it here is what stops the caller's first
        // request racing it.
        transport.ready().await;

        let mut client = McpClient::from_profile(transport, read, write, profile);
        if let Some(handler) = self.input_handler {
            client.set_input_handler(handler);
        }
        Ok(client)
    }
}
