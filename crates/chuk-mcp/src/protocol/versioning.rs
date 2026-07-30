//! Protocol version handling, mirroring `chuk_mcp.protocol.types.versioning`.

use crate::protocol::types::errors::McpError;

/// The stateless revision: no `initialize` handshake, no protocol sessions,
/// per-request `_meta`, and multi round-trip results.
pub const V2026_07_28: &str = "2026-07-28";
pub const V2025_11_25: &str = "2025-11-25";
pub const V2025_06_18: &str = "2025-06-18";
pub const V2025_03_26: &str = "2025-03-26";
pub const V2024_11_05: &str = "2024-11-05";

/// Every protocol version this library supports (newest first).
pub const SUPPORTED_VERSIONS: &[&str] = &[
    V2026_07_28,
    V2025_11_25,
    V2025_06_18,
    V2025_03_26,
    V2024_11_05,
];

/// The versions that use the legacy stateful lifecycle (newest first).
///
/// This — **not** [`SUPPORTED_VERSIONS`] — is what the `initialize` handshake
/// offers. `initialize` does not exist in the 2026-era protocol, so proposing a
/// modern version through it would ask a legacy server for a version it has
/// never heard of.
///
/// `2025-11-25` is **negotiable but not fully implemented**: it is offered so a
/// server whose newest legacy revision is `2025-11-25` (e.g. the reference
/// `mcp` 2.x SDK) negotiates to it rather than being needlessly downgraded to
/// `2025-06-18`. It is driven by the legacy stateful lifecycle; the revision's
/// net-new additive features (icons metadata, URL-mode elicitation, …) are not
/// consumed. See the migration design note.
pub const LEGACY_VERSIONS: &[&str] = &[V2025_11_25, V2025_06_18, V2025_03_26, V2024_11_05];

/// The current/latest supported MCP protocol version.
pub const CURRENT_VERSION: &str = V2026_07_28;

/// The minimum supported MCP protocol version.
pub const MINIMUM_VERSION: &str = V2024_11_05;

/// The first revision using the stateless protocol.
pub const FIRST_MODERN_VERSION: &str = V2026_07_28;

/// The newest revision still using the legacy stateful lifecycle.
pub const LATEST_LEGACY_VERSION: &str = V2025_11_25;

/// Validate that a version follows MCP format (YYYY-MM-DD).
pub fn validate_format(version: &str) -> bool {
    let bytes = version.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7) || b.is_ascii_digit())
}

/// Check if a version is in the supported versions list.
pub fn is_supported(version: &str) -> bool {
    SUPPORTED_VERSIONS.contains(&version)
}

/// Whether `version` uses the stateless 2026-era protocol.
///
/// Classification is by date, not by membership of [`SUPPORTED_VERSIONS`], so a
/// well-formed future revision is treated as modern rather than as legacy. A
/// malformed string is not modern.
pub fn is_modern_version(version: &str) -> bool {
    validate_format(version) && version >= FIRST_MODERN_VERSION
}

/// Parse a version string into (year, month, day).
pub fn parse_version(version: &str) -> Result<(u16, u8, u8), McpError> {
    if !validate_format(version) {
        return Err(McpError::validation(format!(
            "Invalid version format: {version}"
        )));
    }
    let mut parts = version.split('-');
    let year = parts.next().unwrap().parse().unwrap();
    let month = parts.next().unwrap().parse().unwrap();
    let day = parts.next().unwrap().parse().unwrap();
    Ok((year, month, day))
}

/// Compare two versions: -1 / 0 / 1 like the Python API.
pub fn compare(version1: &str, version2: &str) -> Result<i8, McpError> {
    if version1 == version2 {
        return Ok(0);
    }
    for v in [version1, version2] {
        if !validate_format(v) {
            return Err(McpError::validation(format!("Invalid version format: {v}")));
        }
    }
    // Lexicographic comparison works for YYYY-MM-DD.
    Ok(if version1 > version2 { 1 } else { -1 })
}

/// Whether `version1` is newer than `version2`.
pub fn is_newer(version1: &str, version2: &str) -> Result<bool, McpError> {
    Ok(compare(version1, version2)? > 0)
}

/// Whether `version1` is older than `version2`.
pub fn is_older(version1: &str, version2: &str) -> Result<bool, McpError> {
    Ok(compare(version1, version2)? < 0)
}

/// Check if client and server versions are compatible (identical and supported).
pub fn validate_version_compatibility(client_version: &str, server_version: &str) -> bool {
    client_version == server_version && is_supported(client_version)
}

/// Negotiate the best protocol version: the first client version the server
/// also supports.
pub fn negotiate_version(
    client_versions: &[&str],
    server_versions: &[&str],
) -> Result<String, McpError> {
    for client_version in client_versions {
        if server_versions.contains(client_version) {
            return Ok(client_version.to_string());
        }
    }
    Err(McpError::VersionMismatch {
        requested: client_versions.first().unwrap_or(&"unknown").to_string(),
        supported: server_versions.iter().map(|s| s.to_string()).collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_validation() {
        assert!(validate_format("2025-06-18"));
        assert!(!validate_format("2025-6-18"));
        assert!(!validate_format("garbage"));
    }

    #[test]
    fn comparison() {
        assert_eq!(compare("2025-06-18", "2024-11-05").unwrap(), 1);
        assert_eq!(compare("2024-11-05", "2025-06-18").unwrap(), -1);
        assert_eq!(compare("2025-06-18", "2025-06-18").unwrap(), 0);
    }

    #[test]
    fn negotiation() {
        let v = negotiate_version(&["2025-06-18", "2025-03-26"], &["2025-03-26"]).unwrap();
        assert_eq!(v, "2025-03-26");
        assert!(negotiate_version(&["2025-06-18"], &["2019-01-01"]).is_err());
    }

    #[test]
    fn current_version_is_the_stateless_revision() {
        assert_eq!(CURRENT_VERSION, "2026-07-28");
        assert_eq!(SUPPORTED_VERSIONS[0], CURRENT_VERSION);
        assert_eq!(MINIMUM_VERSION, "2024-11-05");
        assert!(SUPPORTED_VERSIONS.iter().all(|v| validate_format(v)));
    }

    #[test]
    fn legacy_versions_exclude_the_modern_revision() {
        // The `initialize` handshake offers this list. A modern version leaking
        // into it would be proposed to legacy servers, which would reject it.
        assert!(!LEGACY_VERSIONS.contains(&FIRST_MODERN_VERSION));
        assert!(LEGACY_VERSIONS.iter().all(|v| !is_modern_version(v)));
        assert_eq!(LEGACY_VERSIONS[0], LATEST_LEGACY_VERSION);
        // Every legacy version is still supported overall.
        assert!(LEGACY_VERSIONS.iter().all(|v| is_supported(v)));
    }

    #[test]
    fn modern_classification() {
        assert!(is_modern_version("2026-07-28"));
        assert!(is_modern_version("2027-01-01"));
        assert!(!is_modern_version("2025-06-18"));
        assert!(!is_modern_version("2024-11-05"));
        assert!(!is_modern_version("garbage"));
        assert!(!is_modern_version(""));
    }

    #[test]
    fn negotiable_2025_11_25_classifies_as_legacy() {
        // 2025-11-25 is negotiable (so we don't needlessly downgrade against a
        // 2025-11-25-capable server) but is driven as legacy — it must never be
        // mistaken for the stateless 2026-era protocol.
        assert!(is_supported("2025-11-25"));
        assert!(!is_modern_version("2025-11-25"));
        assert!(LEGACY_VERSIONS.contains(&V2025_11_25));
        assert_eq!(LATEST_LEGACY_VERSION, V2025_11_25);
    }
}
