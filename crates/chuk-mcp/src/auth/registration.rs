//! Getting a `client_id` from an authorization server.
//!
//! Three mechanisms, tried in the order the specification sets out:
//!
//! 1. **Pre-registration** — credentials the caller already holds for this
//!    server. Nothing to negotiate, so nothing can go wrong.
//! 2. **Client ID Metadata Documents** — the `client_id` *is* an HTTPS URL the
//!    client hosts, which the authorization server fetches. No registration
//!    call, and the identity is portable across servers because each one
//!    resolves it on demand.
//! 3. **Dynamic Client Registration** — a `POST` that mints a client id.
//!    Deprecated, kept for servers that support nothing else.
//!
//! The order is not a preference so much as a narrowing: each step is only
//! reached because the one before it was unavailable.

use serde::Deserialize;
use serde_json::json;

use crate::protocol::types::errors::McpError;

use super::discovery::AuthorizationServerMetadata;
use super::store::Registration;

/// A local redirect URI is a native application, whatever else it is.
///
/// OIDC defaults `application_type` to `"web"`, which forbids exactly the
/// loopback redirect this client uses — so registering without saying so gets
/// the request rejected on a server that implements OIDC.
const APPLICATION_TYPE_NATIVE: &str = "native";

/// Who this client says it is when it registers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientIdentity {
    /// Shown to the user on the consent screen.
    pub client_name: String,
    /// An HTTPS URL hosting this client's metadata document, if it has one.
    ///
    /// Supplying it is what enables Client ID Metadata Documents: the URL
    /// becomes the `client_id`. The document at the far end **MUST** name the
    /// same URL as its own `client_id` and list the redirect URIs this client
    /// will use — an authorization server checks both.
    pub client_id_metadata_url: Option<String>,
    /// A client id already issued by this authorization server.
    pub pre_registered: Option<Registration>,
}

impl Default for ClientIdentity {
    fn default() -> Self {
        ClientIdentity {
            client_name: "chuk-mcp".to_string(),
            client_id_metadata_url: None,
            pre_registered: None,
        }
    }
}

impl ClientIdentity {
    pub fn named(name: impl Into<String>) -> Self {
        ClientIdentity {
            client_name: name.into(),
            ..Default::default()
        }
    }

    /// Use an HTTPS metadata document URL as the `client_id`.
    pub fn with_metadata_document(mut self, url: impl Into<String>) -> Self {
        self.client_id_metadata_url = Some(url.into());
        self
    }

    /// Use credentials this client already holds.
    pub fn with_pre_registered(mut self, client_id: impl Into<String>) -> Self {
        self.pre_registered = Some(Registration {
            client_id: client_id.into(),
            client_secret: None,
        });
        self
    }

    /// Use pre-registered credentials including a secret.
    pub fn with_client_secret(
        mut self,
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
    ) -> Self {
        self.pre_registered = Some(Registration {
            client_id: client_id.into(),
            client_secret: Some(client_secret.into()),
        });
        self
    }
}

/// What a Dynamic Client Registration response carries back.
#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

/// Which mechanism produced a registration.
///
/// Worth knowing because only one of them is worth remembering: a dynamic
/// registration is an identity this authorization server minted and will
/// recognise again, while a metadata-document id is resolved fresh every time
/// and a pre-registered one came from the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    PreRegistered,
    MetadataDocument,
    Dynamic,
}

impl Source {
    /// Whether a registration from this source should be stored for reuse.
    pub fn is_worth_storing(self) -> bool {
        matches!(self, Source::Dynamic)
    }
}

/// Obtain a client id for `metadata`, by whichever mechanism applies.
///
/// `existing` is a registration already stored for this issuer — from a
/// previous dynamic registration against the same authorization server. It is
/// keyed by issuer by the caller, which is what stops a credential minted by
/// one server being presented to another.
pub async fn register(
    client: &reqwest::Client,
    metadata: &AuthorizationServerMetadata,
    identity: &ClientIdentity,
    redirect_uri: &str,
    existing: Option<Registration>,
) -> Result<(Registration, Source), McpError> {
    // 1. Credentials the caller supplied win outright.
    if let Some(pre_registered) = &identity.pre_registered {
        return Ok((pre_registered.clone(), Source::PreRegistered));
    }

    // 2. A metadata document, if this client has one and the server takes them.
    if let Some(url) = &identity.client_id_metadata_url {
        if metadata.client_id_metadata_document_supported == Some(true) {
            tracing::debug!("using the client ID metadata document at {url}");
            return Ok((
                Registration {
                    client_id: url.clone(),
                    client_secret: None,
                },
                Source::MetadataDocument,
            ));
        }
        tracing::debug!(
            "{} does not accept client ID metadata documents; falling back",
            metadata.issuer
        );
    }

    // 3. A registration this client already holds at *this* issuer.
    if let Some(existing) = existing {
        return Ok((existing, Source::Dynamic));
    }

    // 4. Dynamic registration, if the server offers it.
    let Some(endpoint) = &metadata.registration_endpoint else {
        return Err(McpError::validation(format!(
            "{} supports no registration mechanism this client can use: it \
             advertises no registration endpoint, does not accept client ID \
             metadata documents, and no pre-registered client id was supplied",
            metadata.issuer
        )));
    };
    dynamic_register(client, endpoint, identity, redirect_uri).await
}

/// Register dynamically (RFC 7591).
async fn dynamic_register(
    client: &reqwest::Client,
    endpoint: &str,
    identity: &ClientIdentity,
    redirect_uri: &str,
) -> Result<(Registration, Source), McpError> {
    let body = json!({
        "client_name": identity.client_name,
        "redirect_uris": [redirect_uri],
        // Stated rather than defaulted: OIDC would otherwise assume "web" and
        // reject the loopback redirect above.
        "application_type": APPLICATION_TYPE_NATIVE,
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        // A client that cannot keep a secret says so, rather than being
        // issued one it would have to store.
        "token_endpoint_auth_method": "none",
    });

    let response = client
        .post(endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| McpError::Transport(format!("client registration failed: {e}")))?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(McpError::validation(format!(
            "client registration was refused with HTTP {status}: {text}"
        )));
    }

    let registered: RegistrationResponse = serde_json::from_str(&text).map_err(|e| {
        McpError::validation(format!(
            "client registration returned no usable client_id: {e}"
        ))
    })?;

    tracing::debug!("registered dynamically as {}", registered.client_id);
    Ok((
        Registration {
            client_id: registered.client_id,
            client_secret: registered.client_secret,
        },
        Source::Dynamic,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata(json: serde_json::Value) -> AuthorizationServerMetadata {
        serde_json::from_value(json).expect("metadata")
    }

    /// Base metadata with `extra` merged over it.
    fn with(extra: serde_json::Value) -> AuthorizationServerMetadata {
        let mut base = json!({
            "issuer": "https://as.example",
            "authorization_endpoint": "https://as.example/authorize",
            "token_endpoint": "https://as.example/token",
        });
        for (key, value) in extra.as_object().expect("an object") {
            base[key] = value.clone();
        }
        metadata(base)
    }

    async fn choose(
        metadata: &AuthorizationServerMetadata,
        identity: &ClientIdentity,
        existing: Option<Registration>,
    ) -> Result<(Registration, Source), McpError> {
        register(
            &reqwest::Client::new(),
            metadata,
            identity,
            "http://127.0.0.1:1234/callback",
            existing,
        )
        .await
    }

    #[tokio::test]
    async fn pre_registered_credentials_win_over_everything() {
        let identity = ClientIdentity::default()
            .with_client_secret("preset", "shh")
            // Present, and still not used: the caller was explicit.
            .with_metadata_document("https://app.example/client.json");
        let mut identity = identity;
        identity.pre_registered = Some(Registration {
            client_id: "preset".into(),
            client_secret: Some("shh".into()),
        });

        let (registration, source) = choose(
            &with(json!({
                "client_id_metadata_document_supported": true,
                "registration_endpoint": "https://as.example/register",
            })),
            &identity,
            None,
        )
        .await
        .unwrap();

        assert_eq!(source, Source::PreRegistered);
        assert_eq!(registration.client_id, "preset");
        assert_eq!(registration.client_secret.as_deref(), Some("shh"));
    }

    #[tokio::test]
    async fn a_metadata_document_is_used_when_the_server_accepts_one() {
        let identity =
            ClientIdentity::default().with_metadata_document("https://app.example/client.json");
        let (registration, source) = choose(
            &with(json!({"client_id_metadata_document_supported": true})),
            &identity,
            None,
        )
        .await
        .unwrap();

        assert_eq!(source, Source::MetadataDocument);
        // The URL *is* the client id.
        assert_eq!(registration.client_id, "https://app.example/client.json");
        assert!(registration.client_secret.is_none());
    }

    /// A server that does not advertise support must not be sent a URL as a
    /// client id — it would simply be an unknown client.
    #[tokio::test]
    async fn a_metadata_document_is_not_used_when_unsupported() {
        let identity =
            ClientIdentity::default().with_metadata_document("https://app.example/client.json");
        let existing = Registration {
            client_id: "already".into(),
            client_secret: None,
        };

        let (registration, source) = choose(&with(json!({})), &identity, Some(existing))
            .await
            .unwrap();

        assert_eq!(source, Source::Dynamic);
        assert_eq!(registration.client_id, "already");
    }

    #[tokio::test]
    async fn an_existing_registration_is_reused_before_registering_again() {
        let existing = Registration {
            client_id: "from-before".into(),
            client_secret: None,
        };
        let (registration, _) = choose(
            &with(json!({"registration_endpoint": "https://as.example/register"})),
            &ClientIdentity::default(),
            Some(existing),
        )
        .await
        .unwrap();

        assert_eq!(registration.client_id, "from-before");
    }

    /// Nothing to try is an error the caller can act on, not a panic or a
    /// silent anonymous request.
    #[tokio::test]
    async fn no_available_mechanism_is_a_clear_error() {
        let error = choose(&with(json!({})), &ClientIdentity::default(), None)
            .await
            .expect_err("nothing to register with");
        let message = error.to_string();
        assert!(message.contains("registration"), "{message}");
    }

    /// Only a dynamic registration is this server's to remember.
    #[test]
    fn only_dynamic_registrations_are_worth_storing() {
        assert!(Source::Dynamic.is_worth_storing());
        assert!(!Source::MetadataDocument.is_worth_storing());
        assert!(!Source::PreRegistered.is_worth_storing());
    }

    #[test]
    fn an_identity_carries_what_it_was_given() {
        let identity = ClientIdentity::named("my-app")
            .with_metadata_document("https://app.example/c.json")
            .with_pre_registered("abc");

        assert_eq!(identity.client_name, "my-app");
        assert_eq!(
            identity.client_id_metadata_url.as_deref(),
            Some("https://app.example/c.json")
        );
        assert_eq!(identity.pre_registered.unwrap().client_id, "abc");
    }
}
