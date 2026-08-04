//! Shared helpers for the modern result envelope.
//!
//! Every `2026-07-28` result is wrapped in an envelope whose `resultType` says
//! whether the operation ran to completion. A legacy result carries no
//! `resultType`; it normalises upward to [`RESULT_TYPE_COMPLETE`] so a caller
//! reads the same shape whichever era produced the result (design note D4).

/// `resultType` of a result that ran to completion.
pub const RESULT_TYPE_COMPLETE: &str = "complete";

/// Serde default for a `result_type` field: an absent `resultType` (a legacy
/// result) normalises to `"complete"`.
pub(crate) fn default_result_type() -> String {
    RESULT_TYPE_COMPLETE.to_string()
}

/// Field names of the `CacheableResult` interface.
pub(crate) const FIELD_TTL_MS: &str = "ttlMs";
pub(crate) const FIELD_CACHE_SCOPE: &str = "cacheScope";

/// Who may hold a cached response.
///
/// The distinction is about sharing across *authorization contexts*, not about
/// whether caching happens at all: a [`Private`](CacheScope::Private) result is
/// still cached by the client that asked for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheScope {
    /// Contains nothing caller-specific. Any client, gateway or proxy may
    /// store it and serve it to anyone.
    Public,
    /// May be reused only within the same authorization context. A different
    /// access token requires a different cache entry.
    Private,
}

impl CacheScope {
    /// The value this scope goes by on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            CacheScope::Public => "public",
            CacheScope::Private => "private",
        }
    }

    /// Read a scope from its wire value.
    ///
    /// An unrecognised value is not silently treated as `public`: a client
    /// that guessed wrong there would share a private result.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "public" => Some(CacheScope::Public),
            "private" => Some(CacheScope::Private),
            _ => None,
        }
    }
}

/// What one result says about caching itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheHints {
    /// How long the result may be considered fresh, in milliseconds.
    ///
    /// Zero means "immediately stale" — a client may re-fetch whenever it
    /// needs the value. The specification requires this to be `>= 0`, which
    /// the unsigned type enforces.
    pub ttl_ms: u64,
    /// Who may hold the cached response.
    pub scope: CacheScope,
}

impl CacheHints {
    /// Hints with an explicit TTL and scope.
    pub const fn new(ttl_ms: u64, scope: CacheScope) -> Self {
        CacheHints { ttl_ms, scope }
    }

    /// Hints that ask the client to check back every time.
    pub const fn always_stale() -> Self {
        CacheHints::new(0, CacheScope::Private)
    }

    /// Read the hints a server put on a result.
    ///
    /// Absent or malformed fields fall back to the specification's own
    /// defaults rather than to nothing: a missing `ttlMs` means "immediately
    /// stale", a negative one is treated as zero, and a `cacheScope` that is
    /// missing or unrecognised is treated as `private`. That is the reading
    /// that cannot leak a result across authorization contexts, which is the
    /// only way getting this wrong does real damage.
    pub fn read(result: &serde_json::Value) -> Self {
        let ttl_ms = result
            .get(FIELD_TTL_MS)
            .and_then(serde_json::Value::as_i64)
            .map(|ttl| ttl.max(0) as u64)
            .unwrap_or(0);
        let scope = result
            .get(FIELD_CACHE_SCOPE)
            .and_then(serde_json::Value::as_str)
            .and_then(CacheScope::parse)
            .unwrap_or(CacheScope::Private);
        CacheHints { ttl_ms, scope }
    }

    /// Write these hints onto a result object.
    ///
    /// A non-object result is left alone rather than panicking.
    pub fn apply(self, result: &mut serde_json::Value) {
        let Some(object) = result.as_object_mut() else {
            return;
        };
        object.insert(FIELD_TTL_MS.to_string(), serde_json::json!(self.ttl_ms));
        object.insert(
            FIELD_CACHE_SCOPE.to_string(),
            serde_json::json!(self.scope.as_str()),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::completions::CompletionResult;
    use crate::protocol::messages::prompts::GetPromptResult;
    use crate::protocol::messages::resources::ReadResourceResult;
    use serde_json::json;

    #[test]
    fn a_scope_round_trips_through_its_wire_value() {
        assert_eq!(CacheScope::parse("public"), Some(CacheScope::Public));
        assert_eq!(CacheScope::parse("private"), Some(CacheScope::Private));
        // Not silently public: a client that guessed wrong there would share a
        // result meant for one authorization context with another.
        assert_eq!(CacheScope::parse("shared"), None);
        assert_eq!(CacheScope::parse(""), None);
    }

    #[test]
    fn hints_are_read_off_a_result() {
        let hints = CacheHints::read(&json!({"ttlMs": 300_000, "cacheScope": "public"}));
        assert_eq!(hints.ttl_ms, 300_000);
        assert_eq!(hints.scope, CacheScope::Public);
    }

    /// The specification's own fallbacks, and the safe reading of each.
    #[test]
    fn missing_or_malformed_hints_fall_back_safely() {
        // Nothing said: immediately stale, and private.
        let absent = CacheHints::read(&json!({}));
        assert_eq!(absent, CacheHints::always_stale());

        // Negative is treated as zero rather than as a huge unsigned number.
        assert_eq!(CacheHints::read(&json!({"ttlMs": -5})).ttl_ms, 0);

        // An unrecognised scope is private, which cannot leak.
        assert_eq!(
            CacheHints::read(&json!({"cacheScope": "everyone"})).scope,
            CacheScope::Private
        );
        // A ttl that is not a number at all is ignored, not guessed at.
        assert_eq!(CacheHints::read(&json!({"ttlMs": "soon"})).ttl_ms, 0);
    }

    #[test]
    fn applying_hints_to_a_non_object_is_a_no_op() {
        let mut scalar = json!("not an object");
        CacheHints::new(1, CacheScope::Public).apply(&mut scalar);
        assert_eq!(scalar, json!("not an object"));
    }

    #[test]
    fn absent_result_type_normalises_to_complete_across_types() {
        let read: ReadResourceResult = serde_json::from_value(json!({"contents": []})).unwrap();
        assert_eq!(read.result_type, RESULT_TYPE_COMPLETE);

        let prompt: GetPromptResult = serde_json::from_value(json!({})).unwrap();
        assert_eq!(prompt.result_type, RESULT_TYPE_COMPLETE);

        let completion: CompletionResult =
            serde_json::from_value(json!({"values": ["a", "b"]})).unwrap();
        assert_eq!(completion.result_type, RESULT_TYPE_COMPLETE);
    }

    #[test]
    fn present_result_type_is_preserved() {
        let read: ReadResourceResult =
            serde_json::from_value(json!({"contents": [], "resultType": "incomplete"})).unwrap();
        assert_eq!(read.result_type, "incomplete");
    }

    #[test]
    fn value_flattens_each_type() {
        // resource: single text content -> its text
        let read: ReadResourceResult = serde_json::from_value(json!({
            "contents": [{"uri": "x://y", "text": "hello"}]
        }))
        .unwrap();
        assert_eq!(read.value(), json!("hello"));

        // prompt: messages if present
        let prompt: GetPromptResult = serde_json::from_value(json!({
            "messages": [{"role": "user", "content": {"type": "text", "text": "hi"}}]
        }))
        .unwrap();
        assert_eq!(prompt.value().as_array().map(|a| a.len()), Some(1));
        // prompt with no messages -> description
        let desc: GetPromptResult =
            serde_json::from_value(json!({"description": "a prompt"})).unwrap();
        assert_eq!(desc.value(), json!("a prompt"));

        // completion: the values
        let completion: CompletionResult =
            serde_json::from_value(json!({"values": ["x", "y"]})).unwrap();
        assert_eq!(completion.value(), json!(["x", "y"]));
    }
}
