//! Session management, mirroring `chuk_mcp.server.session`.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

/// Information about one client session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionInfo {
    pub session_id: String,
    pub client_info: Map<String, Value>,
    pub protocol_version: String,
    /// Unix timestamp (seconds).
    pub created_at: f64,
    /// Unix timestamp (seconds).
    pub last_activity: f64,
    pub metadata: Map<String, Value>,
}

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs_f64()
}

/// In-memory session manager.
#[derive(Debug, Default)]
pub struct SessionManager {
    sessions: HashMap<String, SessionInfo>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Generate a 32-char hex session id (UUID4 without dashes).
    pub fn generate_session_id() -> String {
        uuid::Uuid::new_v4().simple().to_string()
    }

    /// Create a session and return its id.
    pub fn create_session(
        &mut self,
        client_info: Map<String, Value>,
        protocol_version: &str,
        metadata: Option<Map<String, Value>>,
    ) -> String {
        let session_id = Self::generate_session_id();
        let timestamp = now();
        self.sessions.insert(
            session_id.clone(),
            SessionInfo {
                session_id: session_id.clone(),
                client_info,
                protocol_version: protocol_version.to_string(),
                created_at: timestamp,
                last_activity: timestamp,
                metadata: metadata.unwrap_or_default(),
            },
        );
        session_id
    }

    pub fn get_session(&self, session_id: &str) -> Option<&SessionInfo> {
        self.sessions.get(session_id)
    }

    /// Touch a session's last-activity time. Returns whether it existed.
    pub fn update_activity(&mut self, session_id: &str) -> bool {
        match self.sessions.get_mut(session_id) {
            Some(session) => {
                session.last_activity = now();
                true
            }
            None => false,
        }
    }

    /// Remove sessions idle longer than `max_age_secs`; returns count removed.
    pub fn cleanup_expired(&mut self, max_age_secs: f64) -> usize {
        let cutoff = now() - max_age_secs;
        let before = self.sessions.len();
        self.sessions.retain(|_, s| s.last_activity >= cutoff);
        before - self.sessions.len()
    }

    pub fn list_sessions(&self) -> &HashMap<String, SessionInfo> {
        &self.sessions
    }

    /// Delete a session. Returns whether it existed.
    pub fn delete_session(&mut self, session_id: &str) -> bool {
        self.sessions.remove(session_id).is_some()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Remove all sessions, returning how many there were.
    pub fn clear_all_sessions(&mut self) -> usize {
        let count = self.sessions.len();
        self.sessions.clear();
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_lifecycle() {
        let mut mgr = SessionManager::new();
        let id = mgr.create_session(Map::new(), "2025-06-18", None);
        assert_eq!(id.len(), 32);
        assert!(mgr.get_session(&id).is_some());
        assert!(mgr.update_activity(&id));
        assert_eq!(mgr.session_count(), 1);
        assert!(mgr.delete_session(&id));
        assert!(!mgr.update_activity(&id));
    }

    #[test]
    fn expiry() {
        let mut mgr = SessionManager::new();
        let id = mgr.create_session(Map::new(), "2025-06-18", None);
        assert_eq!(mgr.cleanup_expired(3600.0), 0);
        // Force the session to look old.
        mgr.sessions.get_mut(&id).unwrap().last_activity -= 7200.0;
        assert_eq!(mgr.cleanup_expired(3600.0), 1);
    }

    #[test]
    fn listing_and_clearing() {
        let mut mgr = SessionManager::new();
        assert!(mgr.list_sessions().is_empty());

        let first = mgr.create_session(Map::new(), "2025-06-18", None);
        let second = mgr.create_session(Map::new(), "2024-11-05", None);

        let listed = mgr.list_sessions();
        assert_eq!(listed.len(), 2);
        assert!(listed.contains_key(&first));
        assert!(listed.contains_key(&second));

        assert_eq!(mgr.clear_all_sessions(), 2);
        assert_eq!(mgr.session_count(), 0);
        assert!(mgr.get_session(&first).is_none());
        // Clearing an already-empty manager reports nothing removed.
        assert_eq!(mgr.clear_all_sessions(), 0);
    }
}
