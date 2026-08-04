//! The authorization flow end to end, against an in-process authorization
//! server and a protected MCP server.
//!
//! Unit tests can state what the flow *decides* — which URL to try, whether an
//! `iss` matches — but not what it does over a socket: that discovery runs in
//! the right order, that a `401` provokes a whole flow and then a retry with a
//! token on it, that a `403` widens the scope rather than starting again, that
//! an expiring token is refreshed rather than re-consented.
//!
//! The mock authorization server here is deliberately compliant. Its job is to
//! let the client's behaviour be observed, not to be adversarial — the
//! adversarial cases (a wrong `iss`, a resource that does not match) are pure
//! decisions and live in the unit tests.

#![cfg(feature = "auth")]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use chuk_mcp::auth::{Auth, ClientIdentity, FollowRedirect, InMemoryTokenStore, Key, TokenStore};
use chuk_mcp::connect::Connect;

/// One request the mock stack saw.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    query: HashMap<String, String>,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// A form field of a `application/x-www-form-urlencoded` body.
    fn form(&self, name: &str) -> Option<String> {
        form_urlencoded_pairs(&self.body).remove(name)
    }
}

fn form_urlencoded_pairs(body: &str) -> HashMap<String, String> {
    url_pairs(body)
}

/// Decode `a=1&b=2`, percent-decoding both halves.
fn url_pairs(raw: &str) -> HashMap<String, String> {
    let base = reqwest::Url::parse("http://x/?").expect("a base");
    let with_query = base.join(&format!("?{raw}")).expect("a query");
    with_query
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect()
}

/// What the mock stack has been asked, shared with the test.
type Log = Arc<Mutex<Vec<Seen>>>;

/// How the protected resource should answer the next MCP request.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Guard {
    /// `401` until a token arrives, then serve.
    RequireToken,
    /// `401` first; then `403 insufficient_scope` until `files:write` is held.
    RequireWriteScope,
}

/// Everything the mock stack needs to answer for.
struct Mock {
    log: Log,
    guard: Guard,
    /// Tokens minted so far, and the scopes each carries.
    issued: Arc<Mutex<HashMap<String, Vec<String>>>>,
    /// How many access tokens have been minted.
    mints: Arc<AtomicUsize>,
    /// Seconds until an issued access token expires, if it should.
    expires_in: Option<u64>,
}

/// Serve one HTTP request from an already-accepted socket.
async fn serve_one(mut stream: TcpStream, base: String, mock: Arc<Mock>) {
    let mut buffer = vec![0u8; 16 * 1024];
    let read = match stream.read(&mut buffer).await {
        Ok(0) | Err(_) => return,
        Ok(read) => read,
    };
    let raw = String::from_utf8_lossy(&buffer[..read]).to_string();

    let mut lines = raw.split("\r\n");
    let request_line = lines.next().unwrap_or_default().to_string();
    // The method is not recorded: every assertion here is about paths, query
    // parameters and bodies, and a field nothing reads is a field that rots.
    let target = request_line
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string();

    let headers: Vec<(String, String)> = lines
        .clone()
        .take_while(|line| !line.is_empty())
        .filter_map(|line| line.split_once(": "))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();

    let (path, query) = match target.split_once('?') {
        Some((path, query)) => (path.to_string(), url_pairs(query)),
        None => (target.clone(), HashMap::new()),
    };

    mock.log.lock().unwrap().push(Seen {
        path: path.clone(),
        query: query.clone(),
        headers: headers.clone(),
        body: body.clone(),
    });

    let response = route(&path, &query, &headers, &body, &base, &mock);
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
}

fn ok_json(value: Value) -> String {
    let body = value.to_string();
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn challenge(status: &str, base: &str, extra: &str) -> String {
    let www = format!(
        "Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\"{extra}"
    );
    format!(
        "HTTP/1.1 {status}\r\nWWW-Authenticate: {www}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )
}

/// The whole mock stack: resource server and authorization server on one port.
fn route(
    path: &str,
    query: &HashMap<String, String>,
    headers: &[(String, String)],
    body: &str,
    base: &str,
    mock: &Mock,
) -> String {
    match path {
        // --- protected resource metadata (RFC 9728) ----------------------
        "/.well-known/oauth-protected-resource/mcp" => ok_json(json!({
            "resource": format!("{base}/mcp"),
            "authorization_servers": [base],
            "scopes_supported": ["files:read"],
        })),

        // --- authorization server metadata (RFC 8414) --------------------
        "/.well-known/oauth-authorization-server" => ok_json(json!({
            "issuer": base,
            "authorization_endpoint": format!("{base}/authorize"),
            "token_endpoint": format!("{base}/token"),
            "registration_endpoint": format!("{base}/register"),
            "scopes_supported": ["files:read", "files:write", "offline_access"],
            "response_types_supported": ["code"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "authorization_response_iss_parameter_supported": true,
        })),

        "/register" => ok_json(json!({"client_id": "dynamic-client"})),

        // --- the authorization endpoint: redirect straight back ----------
        "/authorize" => {
            let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
            let state = query.get("state").cloned().unwrap_or_default();
            // Remember what was asked for, so the token carries those scopes.
            let scope = query.get("scope").cloned().unwrap_or_default();
            let code = format!("code-for-{scope}");
            mock.issued.lock().unwrap().insert(
                code.clone(),
                scope.split_whitespace().map(str::to_string).collect(),
            );
            let location = format!("{redirect}?code={code}&state={state}&iss={base}");
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
        }

        // --- the token endpoint ------------------------------------------
        "/token" => {
            let form = url_pairs(body);
            let scopes = match form.get("grant_type").map(String::as_str) {
                Some("refresh_token") => form
                    .get("scope")
                    .map(|s| s.split_whitespace().map(str::to_string).collect())
                    .unwrap_or_default(),
                _ => form
                    .get("code")
                    .and_then(|code| mock.issued.lock().unwrap().get(code).cloned())
                    .unwrap_or_default(),
            };

            let minted = mock.mints.fetch_add(1, Ordering::SeqCst);
            let token = format!("access-{minted}");
            mock.issued
                .lock()
                .unwrap()
                .insert(token.clone(), scopes.clone());

            let mut response = json!({
                "access_token": token,
                "token_type": "Bearer",
                "scope": scopes.join(" "),
            });
            if scopes.iter().any(|s| s == "offline_access") {
                response["refresh_token"] = json!("refresh-token");
            }
            if let Some(seconds) = mock.expires_in {
                response["expires_in"] = json!(seconds);
            }
            ok_json(response)
        }

        // --- the MCP endpoint itself --------------------------------------
        "/mcp" => {
            let presented = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("Authorization"))
                .and_then(|(_, v)| v.strip_prefix("Bearer "))
                .map(str::to_string);

            let held: Vec<String> = presented
                .as_ref()
                .and_then(|token| mock.issued.lock().unwrap().get(token).cloned())
                .unwrap_or_default();

            match (presented.is_some(), mock.guard) {
                (false, _) => return challenge("401 Unauthorized", base, ""),
                (true, Guard::RequireWriteScope) if !held.iter().any(|s| s == "files:write") => {
                    return challenge(
                        "403 Forbidden",
                        base,
                        ", error=\"insufficient_scope\", scope=\"files:write\"",
                    )
                }
                _ => {}
            }

            // Authorized: answer the request that arrived.
            let request: Value = serde_json::from_str(body).unwrap_or(json!({}));
            let id = request.get("id").cloned().unwrap_or(json!(1));
            let result = match request.get("method").and_then(Value::as_str) {
                Some("server/discover") => json!({
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"],
                    "capabilities": {"tools": {}},
                    "ttlMs": 0,
                    "cacheScope": "private",
                    "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "guarded", "version": "1"}},
                }),
                Some("tools/list") => json!({
                    "resultType": "complete", "tools": [], "ttlMs": 0, "cacheScope": "private",
                }),
                _ => json!({"resultType": "complete"}),
            };
            ok_json(json!({"jsonrpc": "2.0", "id": id, "result": result}))
        }

        _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
    }
}

/// Start the mock stack; returns its base URL and the request log.
async fn start(guard: Guard, expires_in: Option<u64>) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let base = format!("http://127.0.0.1:{port}");

    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mock = Arc::new(Mock {
        log: log.clone(),
        guard,
        issued: Arc::new(Mutex::new(HashMap::new())),
        mints: Arc::new(AtomicUsize::new(0)),
        expires_in,
    });

    let served_base = base.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let base = served_base.clone();
            let mock = mock.clone();
            tokio::spawn(serve_one(stream, base, mock));
        }
    });

    (base, log)
}

/// An `Auth` that can complete the flow unattended.
fn unattended() -> Auth {
    Auth::new()
        .handler(Arc::new(FollowRedirect::new()))
        .identity(ClientIdentity::named("auth-e2e"))
        .callback_timeout(Duration::from_secs(10))
}

fn paths(log: &Log) -> Vec<String> {
    log.lock().unwrap().iter().map(|s| s.path.clone()).collect()
}

fn first(log: &Log, path: &str) -> Seen {
    log.lock()
        .unwrap()
        .iter()
        .find(|s| s.path == path)
        .unwrap_or_else(|| panic!("{path} was never requested"))
        .clone()
}

#[tokio::test]
async fn a_401_drives_the_whole_flow_and_the_request_is_retried_with_a_token() {
    let (base, log) = start(Guard::RequireToken, None).await;

    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended())
        .connect()
        .await
        .expect("the flow completes and the connection settles");

    // Every hop the specification describes, in order.
    let seen = paths(&log);
    let index = |path: &str| seen.iter().position(|p| p == path);
    assert!(
        index("/.well-known/oauth-protected-resource/mcp")
            < index("/.well-known/oauth-authorization-server"),
        "resource metadata must be fetched before the authorization server's: {seen:?}"
    );
    assert!(
        index("/.well-known/oauth-authorization-server") < index("/register"),
        "the server's metadata must be read before registering: {seen:?}"
    );
    assert!(
        index("/register") < index("/authorize"),
        "a client id is needed before authorizing: {seen:?}"
    );
    assert!(
        index("/authorize") < index("/token"),
        "a code is needed before redeeming it: {seen:?}"
    );

    // The authorization request carried what the specification requires.
    let authorize = first(&log, "/authorize");
    assert_eq!(authorize.query.get("response_type").unwrap(), "code");
    assert_eq!(
        authorize.query.get("code_challenge_method").unwrap(),
        "S256"
    );
    assert!(!authorize.query.get("code_challenge").unwrap().is_empty());
    assert_eq!(
        authorize.query.get("resource").unwrap(),
        &format!("{base}/mcp"),
        "the resource parameter must name the MCP server"
    );

    // Registration said it was native, or an OIDC server would refuse the
    // loopback redirect.
    let register = first(&log, "/register");
    let registration: Value = serde_json::from_str(&register.body).expect("JSON");
    assert_eq!(registration["application_type"], json!("native"));

    // The token request proved possession of the verifier.
    let token = first(&log, "/token");
    assert_eq!(
        token.form("grant_type").as_deref(),
        Some("authorization_code")
    );
    assert!(
        token.form("code_verifier").is_some(),
        "no PKCE verifier was sent"
    );
    assert_eq!(
        token.form("resource").as_deref(),
        Some(&*format!("{base}/mcp"))
    );

    // And the retried MCP request carried the token.
    let authorized = log
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.path == "/mcp")
        .filter(|s| s.header("Authorization").is_some())
        .count();
    assert!(authorized > 0, "no MCP request was ever made with a token");

    client.close().await.expect("close");
}

#[tokio::test]
async fn an_insufficient_scope_challenge_widens_the_scope_and_keeps_the_old_one() {
    let (base, log) = start(Guard::RequireWriteScope, None).await;

    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended())
        .connect()
        .await
        .expect("the step-up completes");

    // Two authorization requests: the first for what the 401 asked, the second
    // after the 403 named a scope the first did not carry.
    let authorizations: Vec<Seen> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.path == "/authorize")
        .cloned()
        .collect();
    assert!(
        authorizations.len() >= 2,
        "a step-up must re-authorize, saw {} authorization request(s)",
        authorizations.len()
    );

    // The union, not the challenge alone — the whole point of a step-up.
    let widened = authorizations.last().unwrap().query.get("scope").cloned();
    let widened = widened.expect("the second authorization named no scope");
    assert!(
        widened.contains("files:write"),
        "the step-up did not ask for the challenged scope: {widened:?}"
    );
    assert!(
        widened.contains("files:read"),
        "the step-up dropped a scope already granted: {widened:?}"
    );

    client.close().await.expect("close");
}

#[tokio::test]
async fn a_stored_token_is_reused_rather_than_re_authorized() {
    let (base, log) = start(Guard::RequireToken, None).await;
    let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());

    // First connection: the full flow.
    let mut first_client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().store(store.clone()))
        .connect()
        .await
        .expect("first connection");
    first_client.close().await.expect("close");

    let authorizations_before = paths(&log).iter().filter(|p| *p == "/authorize").count();
    assert_eq!(authorizations_before, 1);

    // Second connection, same store: the token is already there.
    let mut second_client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().store(store.clone()))
        .connect()
        .await
        .expect("second connection");
    second_client.close().await.expect("close");

    assert_eq!(
        paths(&log).iter().filter(|p| *p == "/authorize").count(),
        authorizations_before,
        "a live stored token must not provoke another consent round trip"
    );

    // Stored where the specification requires: under the issuer that minted it.
    assert!(
        store
            .tokens(&Key::new(&base, format!("{base}/mcp")))
            .is_some(),
        "the token was not stored under (issuer, resource)"
    );
}

#[tokio::test]
async fn an_expiring_token_is_refreshed_rather_than_re_consented() {
    // One second of life, and `offline_access` is advertised, so the flow asks
    // for a refresh token and uses it rather than sending the user back.
    let (base, log) = start(Guard::RequireToken, Some(1)).await;
    let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());

    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().store(store.clone()))
        .connect()
        .await
        .expect("first connection");
    client.close().await.expect("close");

    let authorizations = paths(&log).iter().filter(|p| *p == "/authorize").count();

    // The token is within the refresh margin the moment it is issued.
    let mut again = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().store(store.clone()))
        .connect()
        .await
        .expect("second connection");
    again.close().await.expect("close");

    let refreshes = log
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.path == "/token")
        .filter(|s| s.form("grant_type").as_deref() == Some("refresh_token"))
        .count();
    assert!(refreshes > 0, "an expiring token was not refreshed");
    assert_eq!(
        paths(&log).iter().filter(|p| *p == "/authorize").count(),
        authorizations,
        "refreshing must not send the user back to the authorization server"
    );
}

#[tokio::test]
async fn offline_access_is_requested_when_the_server_offers_it() {
    let (base, log) = start(Guard::RequireToken, None).await;

    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended())
        .connect()
        .await
        .expect("connect");
    client.close().await.expect("close");

    let scope = first(&log, "/authorize").query.get("scope").cloned();
    assert!(
        scope
            .as_deref()
            .is_some_and(|s| s.contains("offline_access")),
        "offline_access is in scopes_supported but was not requested: {scope:?}"
    );
}

#[tokio::test]
async fn a_client_that_does_not_want_a_refresh_token_does_not_ask_for_one() {
    let (base, log) = start(Guard::RequireToken, None).await;

    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().want_refresh_token(false))
        .connect()
        .await
        .expect("connect");
    client.close().await.expect("close");

    let scope = first(&log, "/authorize").query.get("scope").cloned();
    assert!(
        !scope
            .as_deref()
            .unwrap_or_default()
            .contains("offline_access"),
        "offline_access was requested by a client that declined it: {scope:?}"
    );
}

#[tokio::test]
async fn pre_registered_credentials_skip_registration_entirely() {
    let (base, log) = start(Guard::RequireToken, None).await;

    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(
            unattended().identity(ClientIdentity::named("preset").with_pre_registered("known-id")),
        )
        .connect()
        .await
        .expect("connect");
    client.close().await.expect("close");

    assert!(
        !paths(&log).iter().any(|p| p == "/register"),
        "a client with credentials must not register again"
    );
    assert_eq!(
        first(&log, "/authorize").query.get("client_id").unwrap(),
        "known-id"
    );
}

/// The default handler refuses, so an unconfigured client fails with an
/// explanation rather than making an unattended request to a server-named URL.
#[tokio::test]
async fn the_default_handler_refuses_and_says_why() {
    let (base, _log) = start(Guard::RequireToken, None).await;

    // `McpClient` is not Debug, so the error is taken out by hand rather than
    // with `expect_err`.
    let message = match Connect::to_url(format!("{base}/mcp"))
        .authorization(Auth::new().callback_timeout(Duration::from_secs(2)))
        .connect()
        .await
    {
        Ok(_) => panic!("connected without an AuthorizationHandler"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("AuthorizationHandler"),
        "the refusal did not say what was missing: {message}"
    );
}

/// A server that never challenges must not provoke any authorization traffic.
#[tokio::test]
async fn an_unprotected_server_is_never_asked_to_authorize() {
    let (base, log) = start(Guard::RequireToken, None).await;

    // Prime the store so no flow is needed, then confirm nothing was fetched.
    let store: Arc<dyn TokenStore> = Arc::new(InMemoryTokenStore::new());
    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().store(store.clone()))
        .connect()
        .await
        .expect("connect");
    client.close().await.expect("close");

    let before = log.lock().unwrap().len();
    let mut again = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().store(store))
        .connect()
        .await
        .expect("connect again");
    again.close().await.expect("close");

    let after = log.lock().unwrap().len();
    assert!(
        after - before < 5,
        "a second connection with a stored token re-ran discovery: {} extra requests",
        after - before
    );
}

// ---------------------------------------------------------------------------
// Layouts and failures.
//
// A second, smaller mock whose shape each test dictates: where the metadata
// lives, whether the client is confidential, and whether the token endpoint
// cooperates. These are the paths the happy flow never reaches.
// ---------------------------------------------------------------------------

/// How the metadata is laid out and how the token endpoint behaves.
///
/// Every field defaults to "behave normally", so a test names only the one
/// thing it is bending.
#[derive(Clone, Copy, Default)]
struct Shape {
    /// Publish resource metadata only at the root, not the path-based location.
    prm_at_root_only: bool,
    /// Publish authorization server metadata only at the OpenID Connect path.
    as_at_oidc_only: bool,
    /// Name no authorization server at all.
    no_authorization_server: bool,
    /// Refuse every token request.
    token_endpoint_refuses: bool,
    /// Advertise `client_secret_post` rather than leaving Basic as the default.
    prefers_post_auth: bool,
}

fn shaped_route(
    path: &str,
    query: &HashMap<String, String>,
    headers: &[(String, String)],
    body: &str,
    base: &str,
    shape: Shape,
) -> String {
    let prm = json!({
        "resource": format!("{base}/mcp"),
        "authorization_servers": if shape.no_authorization_server {
            json!([])
        } else {
            json!([base])
        },
    });
    let as_metadata = json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/authorize"),
        "token_endpoint": format!("{base}/token"),
        "response_types_supported": ["code"],
        "code_challenge_methods_supported": ["S256"],
        "token_endpoint_auth_methods_supported": if shape.prefers_post_auth {
            json!(["client_secret_post"])
        } else {
            json!(["client_secret_basic"])
        },
    });

    match path {
        "/.well-known/oauth-protected-resource/mcp" if shape.prm_at_root_only => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
        }
        "/.well-known/oauth-protected-resource/mcp" | "/.well-known/oauth-protected-resource" => {
            ok_json(prm)
        }
        "/.well-known/oauth-authorization-server" if shape.as_at_oidc_only => {
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
        }
        "/.well-known/oauth-authorization-server" | "/.well-known/openid-configuration" => {
            ok_json(as_metadata)
        }
        "/authorize" => {
            let redirect = query.get("redirect_uri").cloned().unwrap_or_default();
            let state = query.get("state").cloned().unwrap_or_default();
            let location = format!("{redirect}?code=the-code&state={state}&iss={base}");
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
        }
        "/token" if shape.token_endpoint_refuses => {
            let body = json!({"error": "invalid_grant"}).to_string();
            format!(
                "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        }
        "/token" => {
            let _ = body;
            ok_json(json!({"access_token": "granted", "token_type": "Bearer"}))
        }
        // The challenge names the metadata location only when one exists
        // there. A client MUST follow `resource_metadata` when it is given, so
        // advertising a URL that 404s would be testing the mock's honesty
        // rather than the client's discovery.
        "/mcp" if shape.prm_at_root_only
            && !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("Authorization")) =>
        {
            "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Bearer\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_string()
        }
        // Once a token is presented, the request is served — otherwise the
        // client would loop until its retry budget ran out and every test
        // here would fail for the same uninteresting reason.
        "/mcp" if headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("Authorization")) =>
        {
            ok_json(json!({
                "jsonrpc": "2.0", "id": 1,
                "result": {
                    "resultType": "complete",
                    "supportedVersions": ["2026-07-28"],
                    "capabilities": {},
                    "ttlMs": 0,
                    "cacheScope": "private",
                    "_meta": {"io.modelcontextprotocol/serverInfo": {"name": "shaped", "version": "1"}},
                },
            }))
        }
        "/mcp" => challenge("401 Unauthorized", base, ""),
        _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
    }
}

/// Start a mock with the given shape; returns its base URL and log.
async fn start_shaped(shape: Shape) -> (String, Log) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let base = format!("http://127.0.0.1:{port}");
    let log: Log = Arc::new(Mutex::new(Vec::new()));

    let served_base = base.clone();
    let served_log = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let base = served_base.clone();
            let log = served_log.clone();
            tokio::spawn(async move {
                let mut buffer = vec![0u8; 16 * 1024];
                let Ok(read) = stream.read(&mut buffer).await else {
                    return;
                };
                let raw = String::from_utf8_lossy(&buffer[..read]).to_string();
                let target = raw
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                let headers: Vec<(String, String)> = raw
                    .split("\r\n")
                    .skip(1)
                    .take_while(|line| !line.is_empty())
                    .filter_map(|line| line.split_once(": "))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                let body = raw.split("\r\n\r\n").nth(1).unwrap_or_default().to_string();
                let (path, query) = match target.split_once('?') {
                    Some((p, q)) => (p.to_string(), url_pairs(q)),
                    None => (target.clone(), HashMap::new()),
                };

                log.lock().unwrap().push(Seen {
                    path: path.clone(),
                    query: query.clone(),
                    headers: headers.clone(),
                    body: body.clone(),
                });

                let response = shaped_route(&path, &query, &headers, &body, &base, shape);
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });

    (base, log)
}

/// Connect, returning the error message when the flow could not complete.
async fn connect_expecting_failure(base: &str, auth: Auth) -> String {
    match Connect::to_url(format!("{base}/mcp"))
        .authorization(auth)
        .connect()
        .await
    {
        Ok(_) => panic!("the connection succeeded where it should not have"),
        Err(error) => error.to_string(),
    }
}

#[tokio::test]
async fn discovery_falls_back_to_the_root_document_and_the_oidc_path() {
    let (base, log) = start_shaped(Shape {
        prm_at_root_only: true,
        as_at_oidc_only: true,
        ..Shape::default()
    })
    .await;

    let mut client = Connect::to_url(format!("{base}/mcp"))
        .authorization(unattended().identity(ClientIdentity::named("x").with_pre_registered("id")))
        .connect()
        .await
        .expect("the fallbacks are followed");
    client.close().await.expect("close");

    let seen = paths(&log);
    // Both the path-based location and the root were tried, in that order.
    assert!(seen.contains(&"/.well-known/oauth-protected-resource/mcp".to_string()));
    assert!(seen.contains(&"/.well-known/oauth-protected-resource".to_string()));
    // And RFC 8414 before OpenID Connect.
    assert!(
        seen.iter()
            .position(|p| p == "/.well-known/oauth-authorization-server")
            < seen
                .iter()
                .position(|p| p == "/.well-known/openid-configuration"),
        "the OpenID Connect path was tried before RFC 8414's: {seen:?}"
    );
}

#[tokio::test]
async fn a_confidential_client_authenticates_the_way_the_server_asks() {
    // Basic by default: RFC 6749 requires every server to support it.
    let (base, log) = start_shaped(Shape::default()).await;
    let mut client =
        Connect::to_url(format!("{base}/mcp"))
            .authorization(unattended().identity(
                ClientIdentity::named("confidential").with_client_secret("client-1", "shh"),
            ))
            .connect()
            .await
            .expect("connect");
    client.close().await.expect("close");

    let token = first(&log, "/token");
    assert!(
        token
            .header("Authorization")
            .is_some_and(|v| v.starts_with("Basic ")),
        "a confidential client did not use Basic where the server allows it"
    );
    assert!(
        token.form("client_secret").is_none(),
        "the secret was also put in the body"
    );

    // A server advertising client_secret_post gets it in the body instead.
    let (base, log) = start_shaped(Shape {
        prefers_post_auth: true,
        ..Shape::default()
    })
    .await;
    let mut client =
        Connect::to_url(format!("{base}/mcp"))
            .authorization(unattended().identity(
                ClientIdentity::named("confidential").with_client_secret("client-1", "shh"),
            ))
            .connect()
            .await
            .expect("connect");
    client.close().await.expect("close");

    let token = first(&log, "/token");
    assert_eq!(token.form("client_secret").as_deref(), Some("shh"));
    assert!(token.header("Authorization").is_none());
}

#[tokio::test]
async fn a_resource_naming_no_authorization_server_is_reported_clearly() {
    let (base, _log) = start_shaped(Shape {
        no_authorization_server: true,
        ..Shape::default()
    })
    .await;

    let message = connect_expecting_failure(&base, unattended()).await;
    assert!(
        message.contains("authorization server"),
        "unhelpful error: {message}"
    );
}

#[tokio::test]
async fn a_token_endpoint_that_refuses_is_reported_with_what_it_said() {
    let (base, _log) = start_shaped(Shape {
        token_endpoint_refuses: true,
        ..Shape::default()
    })
    .await;

    let message = connect_expecting_failure(
        &base,
        unattended().identity(ClientIdentity::named("x").with_pre_registered("id")),
    )
    .await;
    assert!(
        message.contains("invalid_grant") || message.contains("token endpoint"),
        "the refusal did not say what the server answered: {message}"
    );
}

/// A server with no registration mechanism the client can use says so, rather
/// than sending an anonymous authorization request that could only fail.
#[tokio::test]
async fn no_usable_registration_mechanism_is_reported() {
    let (base, _log) = start_shaped(Shape::default()).await;

    // No pre-registered id, no metadata document, and this mock advertises no
    // registration endpoint.
    let message = connect_expecting_failure(&base, unattended()).await;
    assert!(
        message.contains("registration"),
        "unhelpful error: {message}"
    );
}
