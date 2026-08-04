//! PKCE — proving the code came back to whoever asked for it.
//!
//! The client invents a secret, sends only its hash with the authorization
//! request, and reveals the secret when redeeming the code. An attacker who
//! intercepts the authorization code cannot redeem it without the secret,
//! which is the whole point: on a native client the redirect travels through
//! the operating system's URL handling, where interception is realistic.
//!
//! Only `S256` is produced. RFC 7636 also defines `plain`, which offers no
//! protection at all when the challenge is observable — and OAuth 2.1 requires
//! `S256` of any client capable of it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};

/// The challenge method this module implements.
pub const METHOD_S256: &str = "S256";

/// A PKCE secret and the challenge derived from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkce {
    /// The secret, revealed only in the token request.
    pub verifier: String,
    /// `BASE64URL(SHA256(verifier))`, sent with the authorization request.
    pub challenge: String,
}

impl Pkce {
    /// Generate a fresh verifier and its challenge.
    ///
    /// The verifier is 32 random bytes rendered base64url — 43 characters,
    /// comfortably inside RFC 7636's 43..128 range and carrying the full 256
    /// bits, so it cannot be guessed or replayed.
    pub fn generate() -> Self {
        // UUIDs are the crate's existing source of randomness, and two of them
        // is 32 bytes of it. Using them avoids adding an RNG dependency for a
        // single call site.
        let mut bytes = [0u8; 32];
        bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
        Self::from_verifier(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// Derive the challenge for a verifier the caller already has.
    pub fn from_verifier(verifier: impl Into<String>) -> Self {
        let verifier = verifier.into();
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Pkce {
            verifier,
            challenge,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The worked example from RFC 7636 Appendix B. If this holds, the
    /// encoding, the hash and the padding rules are all right.
    #[test]
    fn the_rfc_7636_test_vector_reproduces() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk");
        assert_eq!(
            pkce.challenge,
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn a_generated_verifier_is_the_right_shape() {
        let pkce = Pkce::generate();
        // 32 bytes base64url with no padding.
        assert_eq!(pkce.verifier.len(), 43);
        assert!((43..=128).contains(&pkce.verifier.len()));
        assert!(pkce
            .verifier
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    /// A challenge that is reused is a challenge that can be replayed.
    #[test]
    fn each_generation_is_distinct() {
        let one = Pkce::generate();
        let two = Pkce::generate();
        assert_ne!(one.verifier, two.verifier);
        assert_ne!(one.challenge, two.challenge);
    }

    #[test]
    fn the_challenge_is_url_safe_and_unpadded() {
        let pkce = Pkce::generate();
        assert!(!pkce.challenge.contains('='));
        assert!(!pkce.challenge.contains('+'));
        assert!(!pkce.challenge.contains('/'));
    }

    #[test]
    fn the_same_verifier_always_derives_the_same_challenge() {
        assert_eq!(
            Pkce::from_verifier("abc").challenge,
            Pkce::from_verifier("abc").challenge
        );
    }
}
