//! The rule sets, one module per era-and-subject.
//!
//! Every era-and-subject pair the implementation covers has a module here. An
//! area with no rules would show as an empty row in the rendered matrix, which
//! is where an unimplemented one belongs — not hidden behind rules nobody
//! wrote.

pub mod cross_era;
pub mod legacy_client;
pub mod legacy_server;
pub mod modern_client;
pub mod modern_server;
pub mod mrtr;

use crate::rule::Rule;

/// Every rule the suite knows, in report order.
pub fn all() -> Vec<Rule> {
    let mut rules = Vec::new();
    rules.extend(legacy_client::rules());
    rules.extend(modern_client::rules());
    rules.extend(legacy_server::rules());
    rules.extend(modern_server::rules());
    rules.extend(mrtr::rules());
    rules.extend(cross_era::rules());
    rules
}
