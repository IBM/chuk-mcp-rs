//! Protocol version handling, mirroring `chuk_mcp.protocol.types.versioning`.

use crate::protocol::types::errors::McpError;

/// Supported protocol versions (newest first).
pub const SUPPORTED_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// The current/latest supported MCP protocol version.
pub const CURRENT_VERSION: &str = SUPPORTED_VERSIONS[0];

/// The minimum supported MCP protocol version.
pub const MINIMUM_VERSION: &str = SUPPORTED_VERSIONS[2];

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
}
