//! The rule sets, one module per era-and-subject.
//!
//! There is deliberately no `modern_server` module: this crate's server speaks
//! the legacy lifecycle only. The gap shows up in the rendered matrix, which
//! is where an unimplemented era belongs — not hidden behind a rule that does
//! not exist.

pub mod cross_era;
pub mod legacy_client;
pub mod legacy_server;
pub mod modern_client;

use crate::rule::Rule;

/// Every rule the suite knows, in report order.
pub fn all() -> Vec<Rule> {
    let mut rules = Vec::new();
    rules.extend(legacy_client::rules());
    rules.extend(modern_client::rules());
    rules.extend(legacy_server::rules());
    rules.extend(cross_era::rules());
    rules
}
