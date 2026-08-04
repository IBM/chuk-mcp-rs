//! Caching hints on the results that carry them.
//!
//! The 2026-07-28 revision made MCP stateless, which took away the one thing a
//! client used to rely on to know a list was still current: the connection it
//! negotiated the list over. [SEP-2549] puts that back as an explicit hint —
//! every cacheable result says how long it may be considered fresh (`ttlMs`)
//! and who is allowed to hold it (`cacheScope`).
//!
//! Servers **MUST** include both on `resultType: "complete"` results from
//! `server/discover`, `tools/list`, `prompts/list`, `resources/list`,
//! `resources/templates/list` and `resources/read`. An `input_required`
//! interim result is not cacheable and carries neither.
//!
//! # Why the defaults are conservative
//!
//! `cacheScope` defaults to [`CacheScope::Private`] everywhere. `"public"`
//! tells shared gateways they may serve one caller's response to another, and
//! on an authenticated server that is a data leak rather than an optimisation.
//! Whether a list is genuinely caller-independent is a fact about the server's
//! authorization model, which this library cannot know — so the safe answer is
//! the default and `"public"` is opted into.
//!
//! `ttlMs` defaults to a minute for the three lists that describe the server's
//! own shape, and to zero for the two that return data a caller supplied or
//! owns. Zero is not a refusal to cache: it means "check with me", which is
//! exactly right when the library has no idea how volatile the underlying data
//! is.
//!
//! [SEP-2549]: https://github.com/modelcontextprotocol/modelcontextprotocol/pull/2549

use serde_json::Value;

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::result_envelope::RESULT_TYPE_COMPLETE;

pub use crate::protocol::messages::result_envelope::{CacheHints, CacheScope};

/// The `resultType` field that decides whether a result is cacheable at all.
const FIELD_RESULT_TYPE: &str = "resultType";

/// The default TTL for the results that describe the server's own shape.
///
/// Long enough to be worth having across a burst of calls, short enough that a
/// server whose tools changed without a `listChanged` notification is not
/// misrepresented for long.
const DEFAULT_SHAPE_TTL_MS: u64 = 60_000;

/// What this server says about caching each of the six cacheable operations.
///
/// Every field is public: a server that knows its own data can say so, and the
/// point of the type is to make that a per-operation decision rather than one
/// blanket setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    /// `server/discover` — versions, capabilities and identity.
    pub discover: CacheHints,
    /// `tools/list`.
    pub tools_list: CacheHints,
    /// `prompts/list`.
    pub prompts_list: CacheHints,
    /// `resources/list`.
    pub resources_list: CacheHints,
    /// `resources/templates/list`.
    pub resources_templates_list: CacheHints,
    /// `resources/read`.
    pub resources_read: CacheHints,
}

impl Default for CachePolicy {
    fn default() -> Self {
        let shape = CacheHints::new(DEFAULT_SHAPE_TTL_MS, CacheScope::Private);
        CachePolicy {
            discover: shape,
            tools_list: shape,
            prompts_list: shape,
            resources_templates_list: shape,
            // What a resource *is* and what it *says* both depend on the
            // caller far more often than the tool list does, so neither gets a
            // default TTL it did not ask for.
            resources_list: CacheHints::always_stale(),
            resources_read: CacheHints::always_stale(),
        }
    }
}

impl CachePolicy {
    /// The hints for `method`, or `None` when that method's result is not
    /// cacheable.
    pub fn for_method(&self, method: &str) -> Option<CacheHints> {
        Some(match method {
            MessageMethod::SERVER_DISCOVER => self.discover,
            MessageMethod::TOOLS_LIST => self.tools_list,
            MessageMethod::PROMPTS_LIST => self.prompts_list,
            MessageMethod::RESOURCES_LIST => self.resources_list,
            MessageMethod::RESOURCES_TEMPLATES_LIST => self.resources_templates_list,
            MessageMethod::RESOURCES_READ => self.resources_read,
            _ => return None,
        })
    }

    /// Set every operation's scope, leaving the TTLs alone.
    ///
    /// For a server whose whole surface is caller-independent, which is the
    /// case worth having a shortcut for.
    pub fn with_scope(mut self, scope: CacheScope) -> Self {
        for hints in [
            &mut self.discover,
            &mut self.tools_list,
            &mut self.prompts_list,
            &mut self.resources_list,
            &mut self.resources_templates_list,
            &mut self.resources_read,
        ] {
            hints.scope = scope;
        }
        self
    }

    /// Stamp the hints for `method` onto `result`, if it is cacheable.
    ///
    /// An interim `input_required` result is left alone: it is not a cacheable
    /// answer, it is a request for more input, and the cache key that would
    /// identify it does not exist yet.
    pub fn stamp(&self, method: &str, result: &mut Value) {
        let Some(hints) = self.for_method(method) else {
            return;
        };
        if !is_complete(result) {
            return;
        }
        hints.apply(result);
    }
}

/// Whether this result is a `complete` one.
///
/// A result with no `resultType` at all counts as complete: the field is
/// stamped on modern results elsewhere, and the order the two run in should
/// not decide whether the hints appear.
fn is_complete(result: &Value) -> bool {
    match result.get(FIELD_RESULT_TYPE) {
        Some(Value::String(kind)) => kind == RESULT_TYPE_COMPLETE,
        Some(_) => false,
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scopes_serialise_to_the_words_the_specification_uses() {
        assert_eq!(CacheScope::Public.as_str(), "public");
        assert_eq!(CacheScope::Private.as_str(), "private");
    }

    /// The six operations the specification names, and nothing else.
    #[test]
    fn exactly_the_cacheable_methods_have_hints() {
        let policy = CachePolicy::default();
        for method in [
            MessageMethod::SERVER_DISCOVER,
            MessageMethod::TOOLS_LIST,
            MessageMethod::PROMPTS_LIST,
            MessageMethod::RESOURCES_LIST,
            MessageMethod::RESOURCES_TEMPLATES_LIST,
            MessageMethod::RESOURCES_READ,
        ] {
            assert!(
                policy.for_method(method).is_some(),
                "{method} is cacheable and must have hints"
            );
        }
        for method in [
            MessageMethod::TOOLS_CALL,
            MessageMethod::PROMPTS_GET,
            MessageMethod::COMPLETION_COMPLETE,
            MessageMethod::PING,
            MessageMethod::SUBSCRIPTIONS_LISTEN,
        ] {
            assert!(
                policy.for_method(method).is_none(),
                "{method} is not cacheable and must not carry hints"
            );
        }
    }

    #[test]
    fn stamping_writes_both_fields() {
        let policy = CachePolicy::default();
        let mut result = json!({"tools": []});
        policy.stamp(MessageMethod::TOOLS_LIST, &mut result);

        assert_eq!(result["ttlMs"], json!(DEFAULT_SHAPE_TTL_MS));
        assert_eq!(result["cacheScope"], json!("private"));
    }

    /// The default must be safe on an authenticated server: `"public"` would
    /// let a gateway serve one caller's tool list to another.
    #[test]
    fn every_default_scope_is_private() {
        let policy = CachePolicy::default();
        for method in [
            MessageMethod::SERVER_DISCOVER,
            MessageMethod::TOOLS_LIST,
            MessageMethod::PROMPTS_LIST,
            MessageMethod::RESOURCES_LIST,
            MessageMethod::RESOURCES_TEMPLATES_LIST,
            MessageMethod::RESOURCES_READ,
        ] {
            assert_eq!(
                policy.for_method(method).unwrap().scope,
                CacheScope::Private,
                "{method} must default to private"
            );
        }
    }

    #[test]
    fn with_scope_changes_every_operation() {
        let policy = CachePolicy::default().with_scope(CacheScope::Public);
        let mut result = json!({"resources": []});
        policy.stamp(MessageMethod::RESOURCES_LIST, &mut result);
        assert_eq!(result["cacheScope"], json!("public"));
        // TTLs are left as they were.
        assert_eq!(result["ttlMs"], json!(0));
    }

    /// An interim result is not an answer, so there is nothing to cache and no
    /// cache key that would identify it.
    #[test]
    fn an_input_required_result_gets_no_hints() {
        let policy = CachePolicy::default();
        let mut interim = json!({"resultType": "input_required", "inputRequests": {}});
        policy.stamp(MessageMethod::TOOLS_LIST, &mut interim);

        assert!(interim.get("ttlMs").is_none());
        assert!(interim.get("cacheScope").is_none());
    }

    /// Stamping runs before `resultType` is applied, so a result that has not
    /// been marked yet must still be treated as the complete answer it is.
    #[test]
    fn a_result_without_a_result_type_yet_still_gets_hints() {
        let policy = CachePolicy::default();
        let mut result = json!({"prompts": []});
        policy.stamp(MessageMethod::PROMPTS_LIST, &mut result);
        assert_eq!(result["ttlMs"], json!(DEFAULT_SHAPE_TTL_MS));
    }

    #[test]
    fn a_non_object_result_is_left_alone_rather_than_panicking() {
        let policy = CachePolicy::default();
        let mut scalar = json!("not an object");
        policy.stamp(MessageMethod::TOOLS_LIST, &mut scalar);
        assert_eq!(scalar, json!("not an object"));
    }

    /// `ttlMs` is unsigned, so the "MUST be >= 0" requirement cannot be
    /// violated by construction — this pins the wire form as a JSON integer
    /// rather than a float, which the suite checks for.
    #[test]
    fn ttl_is_written_as_a_non_negative_integer() {
        let mut result = json!({});
        CacheHints::new(300_000, CacheScope::Public).apply(&mut result);
        assert!(result["ttlMs"].is_u64());
        assert_eq!(result["ttlMs"], json!(300_000));
    }
}
