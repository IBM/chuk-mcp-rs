//! Caching of era decisions.
//!
//! Era is a property of the `(endpoint, credential context)` tuple. The
//! credential context is part of the key because the same URL can serve
//! different eras to different principals — a tenant migrated to a modern
//! deployment while others stay on the legacy one, or a gateway that routes by
//! token claim. Keying on endpoint alone would let one principal's detection
//! result decide another's transport behaviour.
//!
//! Entries expire, and are invalidated outright by
//! [`UNSUPPORTED_PROTOCOL_VERSION`], because a server can be upgraded — or
//! rolled back — underneath a running client.
//!
//! [`UNSUPPORTED_PROTOCOL_VERSION`]: crate::protocol::types::errors::UNSUPPORTED_PROTOCOL_VERSION

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::{Detection, ProtocolEra};
use crate::protocol::types::errors::McpError;

/// Default lifetime of a cached era decision.
pub const DEFAULT_ERA_TTL: Duration = Duration::from_secs(300);

/// Identifies the peer an era decision applies to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EndpointKey {
    /// Transport endpoint — a URL for HTTP, a command line for stdio.
    pub endpoint: String,
    /// An opaque, stable identifier for the credential in use.
    ///
    /// **Never a raw token.** Use a stable derived identity — issuer plus
    /// subject, or a hash — so that rotating a token for the same principal
    /// does not silently re-probe, and so that a token cannot leak into
    /// logs or debug output through this key.
    pub credential_context: Option<String>,
}

impl EndpointKey {
    /// A key for an unauthenticated peer.
    pub fn anonymous(endpoint: impl Into<String>) -> Self {
        EndpointKey {
            endpoint: endpoint.into(),
            credential_context: None,
        }
    }

    /// A key scoped to a credential context. See the field docs — this must not
    /// be a raw token.
    pub fn new(endpoint: impl Into<String>, credential_context: impl Into<String>) -> Self {
        EndpointKey {
            endpoint: endpoint.into(),
            credential_context: Some(credential_context.into()),
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    era: ProtocolEra,
    stored_at: Instant,
}

/// A TTL'd store of per-peer era decisions.
#[derive(Debug)]
pub struct EraCache {
    entries: Mutex<HashMap<EndpointKey, Entry>>,
    ttl: Duration,
}

impl EraCache {
    /// A cache with the [`DEFAULT_ERA_TTL`].
    pub fn new() -> Self {
        Self::with_ttl(DEFAULT_ERA_TTL)
    }

    pub fn with_ttl(ttl: Duration) -> Self {
        EraCache {
            entries: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    pub fn ttl(&self) -> Duration {
        self.ttl
    }

    /// The cached era for `key`, if one is present and unexpired.
    pub fn get(&self, key: &EndpointKey) -> Option<ProtocolEra> {
        self.get_at(key, Instant::now())
    }

    /// Cache a decision.
    pub fn insert(&self, key: EndpointKey, era: ProtocolEra) {
        self.insert_at(key, era, Instant::now());
    }

    /// Cache a [`Detection`], ignoring inconclusive ones.
    ///
    /// Returns whether anything was stored. This is the entry point detection
    /// code should use: it makes [`Detection::Undetermined`] un-cacheable by
    /// construction rather than by the caller remembering to check.
    pub fn record(&self, key: EndpointKey, detection: Detection) -> bool {
        match detection {
            Detection::Modern => {
                self.insert(key, ProtocolEra::Modern);
                true
            }
            Detection::Legacy => {
                self.insert(key, ProtocolEra::Legacy);
                true
            }
            Detection::Undetermined => false,
        }
    }

    /// Drop any decision for `key`. Returns whether one was present.
    pub fn invalidate(&self, key: &EndpointKey) -> bool {
        self.lock().remove(key).is_some()
    }

    /// Invalidate `key` if `err` shows the cached era is stale.
    ///
    /// Returns whether an entry was dropped. Call this on every error from a
    /// peer; it is the mechanism that lets a client survive a server being
    /// upgraded mid-session.
    pub fn observe_error(&self, key: &EndpointKey, err: &McpError) -> bool {
        err.invalidates_era() && self.invalidate(key)
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    /// Number of entries held, including any not yet evicted.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn get_at(&self, key: &EndpointKey, now: Instant) -> Option<ProtocolEra> {
        let mut entries = self.lock();
        let entry = *entries.get(key)?;
        if now.saturating_duration_since(entry.stored_at) < self.ttl {
            Some(entry.era)
        } else {
            entries.remove(key);
            None
        }
    }

    fn insert_at(&self, key: EndpointKey, era: ProtocolEra, now: Instant) {
        self.lock().insert(
            key,
            Entry {
                era,
                stored_at: now,
            },
        );
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<EndpointKey, Entry>> {
        self.entries.lock().expect("era cache lock")
    }
}

impl Default for EraCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::types::errors::{
        HEADER_MISMATCH, METHOD_NOT_FOUND, UNSUPPORTED_PROTOCOL_VERSION,
    };

    fn key() -> EndpointKey {
        EndpointKey::anonymous("https://example.test/mcp")
    }

    #[test]
    fn round_trips_a_decision() {
        let cache = EraCache::new();
        assert_eq!(cache.get(&key()), None);
        cache.insert(key(), ProtocolEra::Modern);
        assert_eq!(cache.get(&key()), Some(ProtocolEra::Modern));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn credential_context_partitions_the_cache() {
        // The security-relevant property: one principal's detection must not
        // decide another's era on the same endpoint.
        let cache = EraCache::new();
        let alice = EndpointKey::new("https://example.test/mcp", "issuer|alice");
        let bob = EndpointKey::new("https://example.test/mcp", "issuer|bob");

        cache.insert(alice.clone(), ProtocolEra::Modern);

        assert_eq!(cache.get(&alice), Some(ProtocolEra::Modern));
        assert_eq!(cache.get(&bob), None);
        // ...and neither answers for the anonymous key.
        assert_eq!(cache.get(&key()), None);
    }

    #[test]
    fn entries_expire() {
        let cache = EraCache::with_ttl(Duration::from_secs(60));
        let now = Instant::now();
        cache.insert_at(key(), ProtocolEra::Modern, now);

        assert_eq!(cache.get_at(&key(), now), Some(ProtocolEra::Modern));
        assert_eq!(
            cache.get_at(&key(), now + Duration::from_secs(59)),
            Some(ProtocolEra::Modern)
        );
        assert_eq!(cache.get_at(&key(), now + Duration::from_secs(60)), None);
        // The expired entry is evicted, not merely hidden.
        assert!(cache.is_empty());
    }

    #[test]
    fn unsupported_version_invalidates() {
        let cache = EraCache::new();
        cache.insert(key(), ProtocolEra::Modern);

        // An unrelated error leaves the decision alone.
        let unrelated = McpError::from_json_rpc(METHOD_NOT_FOUND, "nope", None);
        assert!(!cache.observe_error(&key(), &unrelated));
        assert_eq!(cache.get(&key()), Some(ProtocolEra::Modern));

        // Another modern error is a rejection, not an era change.
        let header = McpError::from_json_rpc(HEADER_MISMATCH, "nope", None);
        assert!(!cache.observe_error(&key(), &header));
        assert_eq!(cache.get(&key()), Some(ProtocolEra::Modern));

        // The server was upgraded (or rolled back) underneath us.
        let stale = McpError::from_json_rpc(UNSUPPORTED_PROTOCOL_VERSION, "nope", None);
        assert!(cache.observe_error(&key(), &stale));
        assert_eq!(cache.get(&key()), None);

        // Invalidating an absent key reports nothing dropped.
        assert!(!cache.observe_error(&key(), &stale));
    }

    #[test]
    fn record_refuses_to_store_undetermined() {
        let cache = EraCache::new();

        assert!(!cache.record(key(), Detection::Undetermined));
        assert!(cache.is_empty());

        assert!(cache.record(key(), Detection::Legacy));
        assert_eq!(cache.get(&key()), Some(ProtocolEra::Legacy));

        // A later inconclusive probe must not clobber a good decision either.
        assert!(!cache.record(key(), Detection::Undetermined));
        assert_eq!(cache.get(&key()), Some(ProtocolEra::Legacy));

        assert!(cache.record(key(), Detection::Modern));
        assert_eq!(cache.get(&key()), Some(ProtocolEra::Modern));
    }

    #[test]
    fn invalidate_and_clear() {
        let cache = EraCache::new();
        cache.insert(key(), ProtocolEra::Modern);
        assert!(cache.invalidate(&key()));
        assert!(!cache.invalidate(&key()));

        cache.insert(key(), ProtocolEra::Legacy);
        cache.insert(
            EndpointKey::new("https://other.test/mcp", "issuer|carol"),
            ProtocolEra::Modern,
        );
        assert_eq!(cache.len(), 2);
        cache.clear();
        assert!(cache.is_empty());
    }

    #[test]
    fn default_ttl_is_five_minutes() {
        assert_eq!(EraCache::default().ttl(), Duration::from_secs(300));
    }
}
