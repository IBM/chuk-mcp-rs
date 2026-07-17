//! Version-aware JSON-RPC batching support, mirroring
//! `chuk_mcp.protocol.features.batching`.
//!
//! Batching is supported for protocol versions before `2025-06-18` and
//! removed from `2025-06-18` onward.

use serde_json::{json, Value};

/// Whether the given protocol version supports JSON-RPC batching.
///
/// `None` or malformed versions default to `true` for backward compatibility,
/// matching the Python implementation.
pub fn supports_batching(protocol_version: Option<&str>) -> bool {
    let Some(version) = protocol_version.filter(|v| !v.is_empty()) else {
        return true;
    };

    let parts: Vec<&str> = version.split('-').collect();
    if parts.len() != 3 {
        tracing::warn!("Malformed protocol version '{version}', assuming batching supported");
        return true;
    }

    let (Ok(year), Ok(month), Ok(day)) = (
        parts[0].parse::<u32>(),
        parts[1].parse::<u32>(),
        parts[2].parse::<u32>(),
    ) else {
        tracing::warn!("Unparseable protocol version '{version}', assuming batching supported");
        return true;
    };

    !(year > 2025 || (year == 2025 && month > 6) || (year == 2025 && month == 6 && day >= 18))
}

/// Whether a batch (JSON array) message must be rejected for this version.
pub fn should_reject_batch(protocol_version: Option<&str>, message_data: &Value) -> bool {
    message_data.is_array() && !supports_batching(protocol_version)
}

/// Tracks the negotiated protocol version and whether batching is enabled.
#[derive(Debug, Clone)]
pub struct BatchProcessor {
    pub protocol_version: Option<String>,
    pub batching_enabled: bool,
}

impl Default for BatchProcessor {
    fn default() -> Self {
        BatchProcessor::new(None)
    }
}

impl BatchProcessor {
    pub fn new(protocol_version: Option<String>) -> Self {
        let batching_enabled = supports_batching(protocol_version.as_deref());
        BatchProcessor {
            protocol_version,
            batching_enabled,
        }
    }

    /// Update the protocol version and recompute batching support.
    pub fn update_protocol_version(&mut self, version: &str) {
        let enabled = supports_batching(Some(version));
        if enabled != self.batching_enabled {
            tracing::info!(
                "Batching {} for protocol version {version}",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        self.protocol_version = Some(version.to_string());
        self.batching_enabled = enabled;
    }

    /// Whether this message (single or batch) can be processed.
    pub fn can_process_batch(&self, message_data: &Value) -> bool {
        !message_data.is_array() || self.batching_enabled
    }

    /// Build the JSON-RPC error object sent when a batch is rejected.
    pub fn create_batch_rejection_error(&self, message_id: Option<Value>) -> Value {
        let version = self
            .protocol_version
            .as_deref()
            .unwrap_or("unknown");
        json!({
            "jsonrpc": "2.0",
            "id": message_id,
            "error": {
                "code": -32600,
                "message": format!(
                    "Batch messages are not supported in protocol version {version}"
                ),
                "data": {
                    "protocol_version": version,
                    "batching_supported": false,
                    "upgrade_required": "Batching was removed in protocol version 2025-06-18",
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_boundary() {
        assert!(supports_batching(Some("2024-11-05")));
        assert!(supports_batching(Some("2025-03-26")));
        assert!(!supports_batching(Some("2025-06-18")));
        assert!(!supports_batching(Some("2025-07-01")));
        assert!(!supports_batching(Some("2026-01-01")));
        assert!(supports_batching(None));
        assert!(supports_batching(Some("invalid-version")));
    }

    #[test]
    fn processor_updates() {
        let mut p = BatchProcessor::new(None);
        assert!(p.batching_enabled);
        p.update_protocol_version("2025-06-18");
        assert!(!p.batching_enabled);
        assert!(p.can_process_batch(&json!({"jsonrpc": "2.0"})));
        assert!(!p.can_process_batch(&json!([])));
    }
}
