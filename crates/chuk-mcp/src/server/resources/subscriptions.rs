//! The resources a client has asked to be told about.
//!
//! Recording the interest is the whole of `resources/subscribe`: the server
//! answers an empty result, and what it does with the record is up to whoever
//! notices the resource change.

use std::collections::BTreeSet;
use std::sync::Mutex;

/// The set of subscribed URIs.
///
/// Behind a lock because message handling takes `&self`: several connections
/// may subscribe at once.
#[derive(Debug, Default)]
pub struct Subscriptions {
    uris: Mutex<BTreeSet<String>>,
}

impl Subscriptions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an interest. Returns whether it was new.
    pub fn add(&self, uri: &str) -> bool {
        self.lock().insert(uri.to_string())
    }

    /// Forget an interest. Returns whether there was one.
    pub fn remove(&self, uri: &str) -> bool {
        self.lock().remove(uri)
    }

    /// Whether this URI is subscribed to.
    pub fn contains(&self, uri: &str) -> bool {
        self.lock().contains(uri)
    }

    /// Every subscribed URI, in order.
    pub fn all(&self) -> Vec<String> {
        self.lock().iter().cloned().collect()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.uris
            .lock()
            .expect("the subscription lock is never held across a panic")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interest_is_recorded_and_forgotten() {
        let subscriptions = Subscriptions::new();
        assert!(subscriptions.all().is_empty());

        assert!(subscriptions.add("t://watched"));
        assert!(subscriptions.contains("t://watched"));
        assert_eq!(subscriptions.all(), vec!["t://watched".to_string()]);

        assert!(subscriptions.remove("t://watched"));
        assert!(!subscriptions.contains("t://watched"));
        assert!(subscriptions.all().is_empty());
    }

    #[test]
    fn subscribing_twice_is_not_two_subscriptions() {
        let subscriptions = Subscriptions::new();
        assert!(subscriptions.add("t://x"));
        assert!(!subscriptions.add("t://x"));
        assert_eq!(subscriptions.all().len(), 1);
    }

    #[test]
    fn unsubscribing_from_nothing_reports_that_there_was_nothing() {
        assert!(!Subscriptions::new().remove("t://never"));
    }

    #[test]
    fn several_subscriptions_are_reported_in_order() {
        let subscriptions = Subscriptions::new();
        subscriptions.add("t://b");
        subscriptions.add("t://a");
        assert_eq!(
            subscriptions.all(),
            vec!["t://a".to_string(), "t://b".to_string()]
        );
    }
}
