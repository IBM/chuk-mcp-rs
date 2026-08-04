//! The authorization code flow, end to end.
//!
//! Discovery, registration, the redirect, `iss` validation, the token
//! exchange, and refresh. The pieces each live in their own module; this is
//! where the order and the checks between them are decided.
//!
//! Two of those checks are load-bearing and easy to get subtly wrong, so they
//! are spelled out where they happen: the `iss` comparison ([`validate_iss`])
//! and the scope union on a step-up ([`union_scopes`]).

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use reqwest::Url;
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::protocol::types::errors::McpError;

use super::challenge::Challenge;
use super::discovery::{self, AuthorizationServerMetadata, ProtectedResourceMetadata};
use super::pkce::{Pkce, METHOD_S256};
use super::registration;
use super::store::{Key, Registration, Tokens};
use super::Auth;

/// The scope that asks for a refresh token.
const SCOPE_OFFLINE_ACCESS: &str = "offline_access";

/// Refresh this far ahead of expiry rather than letting a request carry a
/// token that dies in flight.
const REFRESH_MARGIN: Duration = Duration::from_secs(30);

/// What a token endpoint returns.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    /// The scopes actually granted, which may be narrower than those asked for.
    #[serde(default)]
    scope: Option<String>,
}

impl TokenResponse {
    fn into_tokens(self, requested: &[String]) -> Tokens {
        Tokens {
            access_token: self.access_token,
            refresh_token: self.refresh_token,
            // A server that says nothing about scope granted what was asked.
            scopes: match self.scope {
                Some(granted) => granted.split_whitespace().map(str::to_string).collect(),
                None => requested.to_vec(),
            },
            expires_at: self
                .expires_in
                .map(|seconds| Instant::now() + Duration::from_secs(seconds)),
        }
    }
}

/// Everything an authorized connection needs to remember.
pub struct Authorized {
    pub tokens: Tokens,
    pub key: Key,
}

/// Obtain a token for `server_url`, given the challenge that demanded one.
///
/// Reuses a stored token when one is live, refreshes it when it is expiring
/// and refreshable, and runs the full flow otherwise.
pub async fn authorize(
    client: &reqwest::Client,
    auth: &Auth,
    server_url: &str,
    challenge: &Challenge,
    extra_scopes: &[String],
) -> Result<Authorized, McpError> {
    let resource = discovery::canonical_resource(server_url)?;
    let prm = fetch_resource_metadata(client, server_url, challenge).await?;

    // The resource this document describes must be the server being talked
    // to. Without the check, a compromised server could name an authorization
    // server for a resource it does not own and harvest tokens minted for it.
    if !discovery::resource_matches(&prm.resource, server_url) {
        return Err(McpError::validation(format!(
            "protected resource metadata describes {:?}, but this is {server_url:?}; \
             refusing to authorize against a document for a different resource",
            prm.resource
        )));
    }

    let issuer = prm
        .authorization_servers
        .first()
        .ok_or_else(|| {
            McpError::validation(
                "protected resource metadata names no authorization server".to_string(),
            )
        })?
        .clone();

    let metadata = fetch_server_metadata(client, &issuer).await?;
    let key = Key::new(&metadata.issuer, &resource);

    // A live token needs no flow at all.
    if let Some(stored) = auth.store.tokens(&key) {
        let covers = extra_scopes
            .iter()
            .all(|wanted| stored.scopes.iter().any(|held| held == wanted));
        if covers && stored.is_live() && !stored.expires_within(REFRESH_MARGIN) {
            return Ok(Authorized {
                tokens: stored,
                key,
            });
        }
        // Expiring but refreshable, and no new scope is being asked for.
        if covers {
            if let Some(refresh) = stored.refresh_token.clone() {
                match refresh_tokens(client, auth, &metadata, &key, &refresh, &stored.scopes).await
                {
                    Ok(tokens) => return Ok(Authorized { tokens, key }),
                    Err(e) => tracing::debug!("refresh failed, authorizing afresh: {e}"),
                }
            }
        }
        // Whatever was stored is no use now.
        auth.store.clear_tokens(&key);
    }

    let scopes = select_scopes(
        challenge,
        &prm,
        &metadata,
        extra_scopes,
        auth.want_refresh_token,
    );
    let tokens = run_code_flow(client, auth, &metadata, &resource, &scopes).await?;
    auth.store.put_tokens(key.clone(), tokens.clone());
    Ok(Authorized { tokens, key })
}

/// Fetch the protected resource metadata.
///
/// The challenge's `resource_metadata` is authoritative when present; the
/// well-known locations are only probed when the server did not say.
async fn fetch_resource_metadata(
    client: &reqwest::Client,
    server_url: &str,
    challenge: &Challenge,
) -> Result<ProtectedResourceMetadata, McpError> {
    let candidates = match &challenge.resource_metadata {
        Some(url) => vec![url.clone()],
        None => discovery::resource_metadata_candidates(server_url)?,
    };

    let mut last = None;
    for candidate in &candidates {
        match fetch_json::<ProtectedResourceMetadata>(client, candidate).await {
            Ok(metadata) => return Ok(metadata),
            Err(e) => {
                tracing::debug!("no resource metadata at {candidate}: {e}");
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        McpError::validation("no protected resource metadata could be located".to_string())
    }))
}

/// Fetch and validate authorization server metadata.
///
/// Each candidate that returns a document is checked before it is accepted: a
/// document whose `issuer` is not the one the URL was built from is discarded
/// rather than used, per RFC 8414 §3.3.
async fn fetch_server_metadata(
    client: &reqwest::Client,
    issuer: &str,
) -> Result<AuthorizationServerMetadata, McpError> {
    let candidates = discovery::server_metadata_candidates(issuer)?;

    let mut last = None;
    for candidate in &candidates {
        match fetch_json::<AuthorizationServerMetadata>(client, candidate).await {
            Ok(metadata) => {
                if !discovery::issuer_matches(&metadata.issuer, issuer) {
                    // Not a near-miss to tolerate: this is the check that stops
                    // a metadata document redirecting the flow to another server.
                    return Err(McpError::validation(format!(
                        "authorization server metadata at {candidate} declares issuer {:?}, \
                         but was fetched for {issuer:?}; refusing to use it",
                        metadata.issuer
                    )));
                }
                return Ok(metadata);
            }
            Err(e) => {
                tracing::debug!("no authorization server metadata at {candidate}: {e}");
                last = Some(e);
            }
        }
    }
    Err(last.unwrap_or_else(|| {
        McpError::validation(format!(
            "no authorization server metadata found for {issuer}"
        ))
    }))
}

/// Which scopes to ask for.
///
/// The challenge wins when it named any: those are what the current operation
/// needs, and the specification calls them authoritative. Otherwise everything
/// the resource says it supports — and if it says nothing, nothing, because
/// inventing a scope is how a request gets refused for asking too much.
fn select_scopes(
    challenge: &Challenge,
    prm: &ProtectedResourceMetadata,
    metadata: &AuthorizationServerMetadata,
    extra: &[String],
    want_refresh_token: bool,
) -> Vec<String> {
    let mut scopes = challenge.scopes();
    if scopes.is_empty() {
        scopes = prm.scopes_supported.clone().unwrap_or_default();
    }
    scopes = union_scopes(&scopes, extra);

    // Only when the server offers it: asking for a scope that is not
    // advertised is how an authorization request gets rejected outright.
    if want_refresh_token
        && metadata.offers_scope(SCOPE_OFFLINE_ACCESS)
        && !scopes.iter().any(|s| s == SCOPE_OFFLINE_ACCESS)
    {
        scopes.push(SCOPE_OFFLINE_ACCESS.to_string());
    }
    scopes
}

/// Combine two scope sets, preserving order and dropping duplicates.
///
/// The union is what makes a step-up non-destructive: a server challenging for
/// `files:write` is stating what *this* operation needs, not restating
/// everything already granted, so re-authorizing with only the challenge would
/// quietly drop permissions the client still needs elsewhere.
pub fn union_scopes(held: &[String], wanted: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    held.iter()
        .chain(wanted)
        .filter(|scope| seen.insert((*scope).clone()))
        .cloned()
        .collect()
}

/// Run the authorization code flow and return the tokens it yields.
async fn run_code_flow(
    client: &reqwest::Client,
    auth: &Auth,
    metadata: &AuthorizationServerMetadata,
    resource: &str,
    scopes: &[String],
) -> Result<Tokens, McpError> {
    // Bound before the user is sent anywhere: the redirect must have somewhere
    // to land the moment the authorization server issues it.
    let listener = CallbackListener::bind().await?;
    let redirect_uri = listener.redirect_uri();

    let existing = auth.store.registration(&metadata.issuer);
    let (registration, source) =
        registration::register(client, metadata, &auth.identity, &redirect_uri, existing).await?;
    if source.is_worth_storing() {
        auth.store
            .put_registration(metadata.issuer.clone(), registration.clone());
    }

    let pkce = Pkce::generate();
    let state = uuid::Uuid::new_v4().to_string();
    let url = authorization_url(
        metadata,
        &registration,
        &redirect_uri,
        resource,
        scopes,
        &pkce,
        &state,
    )?;

    // The handler runs *concurrently* with the wait, never before it. A
    // handler that drives the redirect itself — [`super::FollowRedirect`], or
    // anything else that follows the chain in-process — only finishes once the
    // callback has been answered, and the callback is answered here. Awaiting
    // the handler first would deadlock the two against each other.
    let handler = auth.handler.clone();
    let target = url.to_string();
    let authorizing = tokio::spawn(async move { handler.authorize(&target).await });

    let callback = match listener.wait(auth.callback_timeout).await {
        Ok(callback) => callback,
        Err(waiting_failed) => {
            // A handler that refused explains the silence better than the
            // timeout does, so it is preferred when there is one.
            return Err(match authorizing.await {
                Ok(Err(refused)) => refused,
                _ => waiting_failed,
            });
        }
    };

    if callback.state.as_deref() != Some(state.as_str()) {
        return Err(McpError::validation(
            "the authorization response carried the wrong state; refusing it".to_string(),
        ));
    }
    validate_iss(metadata, callback.iss.as_deref())?;

    // Checked only after `iss`: the specification is explicit that a client
    // must not act on or display an error whose issuer it could not verify.
    if let Some(error) = &callback.error {
        return Err(McpError::validation(format!(
            "the authorization server refused: {error}"
        )));
    }
    let code = callback.code.ok_or_else(|| {
        McpError::validation("the authorization response carried no code".to_string())
    })?;

    exchange_code(
        client,
        metadata,
        &registration,
        &code,
        &pkce.verifier,
        &redirect_uri,
        resource,
        scopes,
    )
    .await
}

/// Apply RFC 9207 §2.4 to the `iss` of an authorization response.
///
/// The four cases, and why the asymmetry:
///
/// | advertised | `iss` present | action |
/// |---|---|---|
/// | yes | yes | compare |
/// | yes | no  | **reject** — the server promised one, so its absence is a sign of tampering |
/// | no  | yes | compare anyway — a server may emit `iss` before updating its metadata |
/// | no  | no  | proceed |
///
/// The comparison is a plain string equality. No normalisation of any kind:
/// case folding, default-port elision or a trailing slash would each make a
/// different issuer compare equal, which is the exact substitution this check
/// exists to catch.
pub fn validate_iss(
    metadata: &AuthorizationServerMetadata,
    iss: Option<&str>,
) -> Result<(), McpError> {
    let advertised = metadata
        .authorization_response_iss_parameter_supported
        .unwrap_or(false);

    match (advertised, iss) {
        (_, Some(iss)) if iss == metadata.issuer => Ok(()),
        (_, Some(iss)) => Err(McpError::validation(format!(
            "the authorization response was issued by {iss:?}, but this flow was started \
             with {:?}; refusing it",
            metadata.issuer
        ))),
        (true, None) => Err(McpError::validation(format!(
            "{} advertises that it sends the iss parameter but the authorization \
             response carried none; refusing it",
            metadata.issuer
        ))),
        (false, None) => Ok(()),
    }
}

/// Build the authorization request URL.
fn authorization_url(
    metadata: &AuthorizationServerMetadata,
    registration: &Registration,
    redirect_uri: &str,
    resource: &str,
    scopes: &[String],
    pkce: &Pkce,
    state: &str,
) -> Result<Url, McpError> {
    let mut url = Url::parse(&metadata.authorization_endpoint)
        .map_err(|e| McpError::validation(format!("unusable authorization endpoint: {e}")))?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", &registration.client_id);
        query.append_pair("redirect_uri", redirect_uri);
        query.append_pair("state", state);
        query.append_pair("code_challenge", &pkce.challenge);
        query.append_pair("code_challenge_method", METHOD_S256);
        // Sent whether or not the server advertises support for it: the
        // specification requires it unconditionally, and a server that does
        // not understand it ignores it.
        query.append_pair("resource", resource);
        if !scopes.is_empty() {
            query.append_pair("scope", &scopes.join(" "));
        }
    }
    Ok(url)
}

/// Redeem an authorization code.
#[allow(clippy::too_many_arguments)]
async fn exchange_code(
    client: &reqwest::Client,
    metadata: &AuthorizationServerMetadata,
    registration: &Registration,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    resource: &str,
    scopes: &[String],
) -> Result<Tokens, McpError> {
    let form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), redirect_uri.to_string()),
        ("code_verifier".to_string(), verifier.to_string()),
        ("resource".to_string(), resource.to_string()),
    ];
    post_token_request(client, metadata, registration, form, scopes).await
}

/// Trade a refresh token for a new access token.
async fn refresh_tokens(
    client: &reqwest::Client,
    auth: &Auth,
    metadata: &AuthorizationServerMetadata,
    key: &Key,
    refresh_token: &str,
    scopes: &[String],
) -> Result<Tokens, McpError> {
    let registration = auth
        .store
        .registration(&metadata.issuer)
        .or_else(|| auth.identity.pre_registered.clone())
        .or_else(|| {
            auth.identity
                .client_id_metadata_url
                .as_ref()
                .map(|url| Registration {
                    client_id: url.clone(),
                    client_secret: None,
                })
        })
        .ok_or_else(|| {
            McpError::validation("no client registration to refresh with".to_string())
        })?;

    let form = vec![
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token.to_string()),
        ("resource".to_string(), key.resource.clone()),
    ];
    let mut tokens = post_token_request(client, metadata, &registration, form, scopes).await?;

    // A server that rotates refresh tokens returns a new one; one that does
    // not expects the old to keep working, so it is carried forward rather
    // than lost.
    if tokens.refresh_token.is_none() {
        tokens.refresh_token = Some(refresh_token.to_string());
    }
    auth.store.put_tokens(key.clone(), tokens.clone());
    Ok(tokens)
}

/// Send a token request, authenticating the client however the server wants.
///
/// Three methods, chosen by what the server advertises. A public client with
/// no secret simply names itself in the body — `none` — which is what a
/// metadata-document or dynamically-registered client normally is.
async fn post_token_request(
    client: &reqwest::Client,
    metadata: &AuthorizationServerMetadata,
    registration: &Registration,
    mut form: Vec<(String, String)>,
    scopes: &[String],
) -> Result<Tokens, McpError> {
    if !scopes.is_empty() {
        form.push(("scope".to_string(), scopes.join(" ")));
    }

    let mut request = client.post(&metadata.token_endpoint);
    match &registration.client_secret {
        // HTTP Basic is the default RFC 6749 requires a server to support, so
        // it is preferred whenever a secret exists and the server allows it.
        Some(secret) if !metadata.supports_auth_method("client_secret_post") => {
            request = request.basic_auth(&registration.client_id, Some(secret));
        }
        Some(secret) => {
            form.push(("client_id".to_string(), registration.client_id.clone()));
            form.push(("client_secret".to_string(), secret.clone()));
        }
        None => {
            form.push(("client_id".to_string(), registration.client_id.clone()));
        }
    }

    let response = request
        .form(&form)
        .send()
        .await
        .map_err(|e| McpError::Transport(format!("token request failed: {e}")))?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(McpError::validation(format!(
            "the token endpoint refused with HTTP {status}: {text}"
        )));
    }

    let parsed: TokenResponse = serde_json::from_str(&text)
        .map_err(|e| McpError::validation(format!("unusable token response: {e}")))?;
    Ok(parsed.into_tokens(scopes))
}

/// Fetch and decode a JSON metadata document.
async fn fetch_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T, McpError> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| McpError::Transport(format!("could not fetch {url}: {e}")))?;

    if !response.status().is_success() {
        return Err(McpError::validation(format!(
            "{url} answered HTTP {}",
            response.status()
        )));
    }
    let text = response
        .text()
        .await
        .map_err(|e| McpError::Transport(format!("could not read {url}: {e}")))?;
    serde_json::from_str(&text)
        .map_err(|e| McpError::validation(format!("{url} returned unusable JSON: {e}")))
}

/// What came back on the redirect.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Callback {
    pub code: Option<String>,
    pub state: Option<String>,
    pub iss: Option<String>,
    pub error: Option<String>,
}

/// A one-shot loopback listener for the redirect.
///
/// Loopback rather than a public URL because this is a native client: the
/// authorization server redirects the user's browser to `127.0.0.1`, which
/// only this process can be listening on.
struct CallbackListener {
    listener: TcpListener,
    port: u16,
}

impl CallbackListener {
    async fn bind() -> Result<Self, McpError> {
        // Port zero: the operating system picks a free one, so two flows can
        // run at once and neither collides with whatever else is listening.
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|e| McpError::Transport(format!("could not open a callback listener: {e}")))?;
        let port = listener
            .local_addr()
            .map_err(|e| McpError::Transport(format!("callback listener has no address: {e}")))?
            .port();
        Ok(CallbackListener { listener, port })
    }

    fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/callback", self.port)
    }

    /// Wait for the browser to arrive, and answer it with something readable.
    ///
    /// Loops rather than accepting once. A browser opening a page makes more
    /// connections than the one carrying the callback — a favicon fetch, a
    /// speculative preconnect — and any of them can arrive first. Taking the
    /// first connection as the answer would read a `GET /favicon.ico` as a
    /// failed authorization.
    async fn wait(&self, timeout: Duration) -> Result<Callback, McpError> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Err(McpError::Transport(format!(
                    "no authorization callback arrived within {timeout:?}"
                )));
            }

            let accepted = tokio::time::timeout(remaining, self.listener.accept())
                .await
                .map_err(|_| {
                    McpError::Transport(format!(
                        "no authorization callback arrived within {timeout:?}"
                    ))
                })?;
            let (mut stream, _) = accepted
                .map_err(|e| McpError::Transport(format!("callback connection failed: {e}")))?;

            // The request line carries the query, and that is all this needs.
            let mut buffer = [0u8; 8192];
            let read = stream
                .read(&mut buffer)
                .await
                .map_err(|e| McpError::Transport(format!("could not read the callback: {e}")))?;
            let request = String::from_utf8_lossy(&buffer[..read]);
            let callback = parse_callback(&request).unwrap_or_default();

            // Only a request that actually carries an authorization result
            // ends the wait; anything else is answered and ignored.
            let is_callback = callback.code.is_some() || callback.error.is_some();
            let body = if !is_callback {
                "Waiting for the authorization callback."
            } else if callback.error.is_none() {
                "Authorization complete. You can close this window."
            } else {
                "Authorization failed. You can close this window."
            };

            let _ = stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = stream.shutdown().await;

            if is_callback {
                return Ok(callback);
            }
            tracing::debug!("ignored a connection to the callback listener that carried no result");
        }
    }
}

/// Read the query parameters out of a raw HTTP request.
pub fn parse_callback(request: &str) -> Result<Callback, McpError> {
    let target = request.split_whitespace().nth(1).ok_or_else(|| {
        McpError::validation("the callback carried no request target".to_string())
    })?;

    // Resolved against a dummy base so a path-only target parses; only the
    // query is read from it.
    let url = Url::parse("http://127.0.0.1")
        .and_then(|base| base.join(target))
        .map_err(|e| McpError::validation(format!("unusable callback target: {e}")))?;

    let mut callback = Callback::default();
    for (key, value) in url.query_pairs() {
        match key.as_ref() {
            "code" => callback.code = Some(value.into_owned()),
            "state" => callback.state = Some(value.into_owned()),
            "iss" => callback.iss = Some(value.into_owned()),
            "error" => callback.error = Some(value.into_owned()),
            _ => {}
        }
    }
    Ok(callback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata(extra: serde_json::Value) -> AuthorizationServerMetadata {
        let mut base = json!({
            "issuer": "https://as.example",
            "authorization_endpoint": "https://as.example/authorize",
            "token_endpoint": "https://as.example/token",
        });
        for (k, v) in extra.as_object().unwrap() {
            base[k] = v.clone();
        }
        serde_json::from_value(base).unwrap()
    }

    // --- iss validation, the four rows of the specification's table -------

    #[test]
    fn advertised_and_matching_is_accepted() {
        let metadata = metadata(json!({"authorization_response_iss_parameter_supported": true}));
        assert!(validate_iss(&metadata, Some("https://as.example")).is_ok());
    }

    /// A server that promised `iss` and did not send it may have had the
    /// response tampered with.
    #[test]
    fn advertised_and_missing_is_rejected() {
        let metadata = metadata(json!({"authorization_response_iss_parameter_supported": true}));
        assert!(validate_iss(&metadata, None).is_err());
    }

    /// The local-policy row: compare whenever one arrives, advertised or not.
    #[test]
    fn unadvertised_but_present_is_still_compared() {
        let metadata = metadata(json!({}));
        assert!(validate_iss(&metadata, Some("https://as.example")).is_ok());
        assert!(validate_iss(&metadata, Some("https://evil.example")).is_err());
    }

    #[test]
    fn neither_advertised_nor_present_proceeds() {
        assert!(validate_iss(&metadata(json!({})), None).is_ok());
        let explicit = metadata(json!({"authorization_response_iss_parameter_supported": false}));
        assert!(validate_iss(&explicit, None).is_ok());
    }

    #[test]
    fn a_wrong_issuer_is_rejected_however_it_was_advertised() {
        for advertised in [json!(true), json!(false)] {
            let metadata =
                metadata(json!({"authorization_response_iss_parameter_supported": advertised}));
            assert!(validate_iss(&metadata, Some("https://evil.example")).is_err());
        }
    }

    /// No normalisation: a trailing slash is a different issuer. This is the
    /// case the specification calls out by name.
    #[test]
    fn a_normalised_issuer_does_not_compare_equal() {
        let metadata = metadata(json!({"authorization_response_iss_parameter_supported": true}));
        assert!(validate_iss(&metadata, Some("https://as.example/")).is_err());
        assert!(validate_iss(&metadata, Some("HTTPS://AS.EXAMPLE")).is_err());
    }

    // --- scope selection --------------------------------------------------

    fn prm(scopes: Option<Vec<&str>>) -> ProtectedResourceMetadata {
        ProtectedResourceMetadata {
            resource: "https://mcp.example/mcp".into(),
            authorization_servers: vec!["https://as.example".into()],
            scopes_supported: scopes.map(|s| s.into_iter().map(str::to_string).collect::<Vec<_>>()),
        }
    }

    fn challenge_with(scope: Option<&str>) -> Challenge {
        Challenge {
            scope: scope.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn the_challenge_scope_wins_when_there_is_one() {
        let scopes = select_scopes(
            &challenge_with(Some("files:read")),
            &prm(Some(vec!["everything"])),
            &metadata(json!({})),
            &[],
            false,
        );
        assert_eq!(scopes, vec!["files:read"]);
    }

    #[test]
    fn scopes_supported_is_used_when_the_challenge_named_none() {
        let scopes = select_scopes(
            &challenge_with(None),
            &prm(Some(vec!["a", "b"])),
            &metadata(json!({})),
            &[],
            false,
        );
        assert_eq!(scopes, vec!["a", "b"]);
    }

    /// Nothing advertised means no `scope` parameter at all — asking for a
    /// scope nobody defined is how a request gets refused.
    #[test]
    fn no_scope_is_requested_when_none_is_defined() {
        let scopes = select_scopes(
            &challenge_with(None),
            &prm(None),
            &metadata(json!({})),
            &[],
            false,
        );
        assert!(scopes.is_empty());
    }

    #[test]
    fn offline_access_is_added_only_when_the_server_offers_it() {
        let offering = metadata(json!({"scopes_supported": ["a", "offline_access"]}));
        let scopes = select_scopes(&challenge_with(Some("a")), &prm(None), &offering, &[], true);
        assert!(scopes.contains(&"offline_access".to_string()));

        let silent = metadata(json!({"scopes_supported": ["a"]}));
        let scopes = select_scopes(&challenge_with(Some("a")), &prm(None), &silent, &[], true);
        assert!(!scopes.contains(&"offline_access".to_string()));
    }

    #[test]
    fn offline_access_is_not_requested_when_it_is_not_wanted() {
        let offering = metadata(json!({"scopes_supported": ["a", "offline_access"]}));
        let scopes = select_scopes(
            &challenge_with(Some("a")),
            &prm(None),
            &offering,
            &[],
            false,
        );
        assert!(!scopes.contains(&"offline_access".to_string()));
    }

    /// What makes a step-up non-destructive.
    #[test]
    fn a_union_keeps_what_was_already_granted() {
        let held = vec!["files:read".to_string()];
        let wanted = vec!["files:write".to_string()];
        assert_eq!(
            union_scopes(&held, &wanted),
            vec!["files:read", "files:write"]
        );
    }

    #[test]
    fn a_union_does_not_repeat_a_scope() {
        let held = vec!["a".to_string(), "b".to_string()];
        let wanted = vec!["b".to_string(), "c".to_string()];
        assert_eq!(union_scopes(&held, &wanted), vec!["a", "b", "c"]);
    }

    // --- the authorization URL --------------------------------------------

    #[test]
    fn the_authorization_url_carries_every_required_parameter() {
        let url = authorization_url(
            &metadata(json!({})),
            &Registration {
                client_id: "client-1".into(),
                client_secret: None,
            },
            "http://127.0.0.1:9999/callback",
            "https://mcp.example/mcp",
            &["files:read".to_string()],
            &Pkce::from_verifier("verifier-verifier-verifier-verifier-1"),
            "state-1",
        )
        .unwrap();

        let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], "client-1");
        assert_eq!(query["redirect_uri"], "http://127.0.0.1:9999/callback");
        assert_eq!(query["state"], "state-1");
        assert_eq!(query["code_challenge_method"], "S256");
        assert!(!query["code_challenge"].is_empty());
        // Required unconditionally by the specification.
        assert_eq!(query["resource"], "https://mcp.example/mcp");
        assert_eq!(query["scope"], "files:read");
    }

    /// An empty scope set means the parameter is absent, not empty.
    #[test]
    fn no_scope_parameter_is_sent_when_there_are_no_scopes() {
        let url = authorization_url(
            &metadata(json!({})),
            &Registration {
                client_id: "c".into(),
                client_secret: None,
            },
            "http://127.0.0.1:1/callback",
            "https://mcp.example/mcp",
            &[],
            &Pkce::generate(),
            "s",
        )
        .unwrap();
        assert!(url.query_pairs().all(|(k, _)| k != "scope"));
    }

    // --- the callback -----------------------------------------------------

    #[test]
    fn a_callback_request_yields_its_parameters() {
        let callback = parse_callback(
            "GET /callback?code=abc&state=xyz&iss=https%3A%2F%2Fas.example HTTP/1.1\r\nHost: x\r\n\r\n",
        )
        .unwrap();

        assert_eq!(callback.code.as_deref(), Some("abc"));
        assert_eq!(callback.state.as_deref(), Some("xyz"));
        assert_eq!(callback.iss.as_deref(), Some("https://as.example"));
        assert!(callback.error.is_none());
    }

    #[test]
    fn an_error_callback_is_read_as_one() {
        let callback =
            parse_callback("GET /callback?error=access_denied&state=s HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(callback.error.as_deref(), Some("access_denied"));
        assert!(callback.code.is_none());
    }

    #[test]
    fn a_callback_with_no_query_carries_nothing() {
        let callback = parse_callback("GET /callback HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(callback, Callback::default());
    }

    #[test]
    fn a_malformed_request_line_is_an_error() {
        assert!(parse_callback("").is_err());
    }

    #[tokio::test]
    async fn a_listener_binds_loopback_and_names_itself() {
        let listener = CallbackListener::bind().await.unwrap();
        let uri = listener.redirect_uri();
        assert!(uri.starts_with("http://127.0.0.1:"));
        assert!(uri.ends_with("/callback"));
    }

    /// A user who never comes back must not hold the connection open forever.
    #[tokio::test]
    async fn a_callback_that_never_arrives_times_out() {
        let listener = CallbackListener::bind().await.unwrap();
        let error = listener
            .wait(Duration::from_millis(50))
            .await
            .expect_err("nothing arrived");
        assert!(error.to_string().contains("callback"), "{error}");
    }

    /// The end-to-end shape of the redirect leg, without a browser.
    #[tokio::test]
    async fn a_redirect_is_read_off_the_listener() {
        let listener = CallbackListener::bind().await.unwrap();
        let uri = listener.redirect_uri();

        tokio::spawn(async move {
            let _ = reqwest::Client::new()
                .get(format!(
                    "{uri}?code=the-code&state=the-state&iss=https://as.example"
                ))
                .send()
                .await;
        });

        let callback = listener.wait(Duration::from_secs(5)).await.unwrap();
        assert_eq!(callback.code.as_deref(), Some("the-code"));
        assert_eq!(callback.state.as_deref(), Some("the-state"));
        assert_eq!(callback.iss.as_deref(), Some("https://as.example"));
    }

    #[test]
    fn a_token_response_without_a_scope_keeps_what_was_asked_for() {
        let response: TokenResponse =
            serde_json::from_value(json!({"access_token": "t", "expires_in": 3600})).unwrap();
        let tokens = response.into_tokens(&["a".to_string(), "b".to_string()]);

        assert_eq!(tokens.scopes, vec!["a", "b"]);
        assert!(tokens.is_live());
        assert!(tokens.expires_at.is_some());
    }

    #[test]
    fn a_token_response_with_a_scope_reports_what_was_granted() {
        let response: TokenResponse =
            serde_json::from_value(json!({"access_token": "t", "scope": "a"})).unwrap();
        let tokens = response.into_tokens(&["a".to_string(), "b".to_string()]);

        // Narrower than asked for, and that is what is recorded.
        assert_eq!(tokens.scopes, vec!["a"]);
        assert!(tokens.expires_at.is_none());
    }
}
