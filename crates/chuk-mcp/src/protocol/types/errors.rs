//! MCP protocol error codes and the library error type, mirroring
//! `chuk_mcp.protocol.types.errors`.

use serde_json::Value;

// Standard JSON-RPC 2.0 error codes
pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;

// SDK error codes
pub const CONNECTION_CLOSED: i64 = -32000;
pub const REQUEST_TIMEOUT: i64 = -32001;

// Server error code range (reserved -32000..=-32099)
pub const SERVER_ERROR_START: i64 = -32000;
pub const SERVER_ERROR_END: i64 = -32099;

// MCP-specific error codes (in the server error range)
pub const MCP_INITIALIZATION_FAILED: i64 = -32002;
pub const MCP_CAPABILITY_NOT_SUPPORTED: i64 = -32003;
pub const MCP_RESOURCE_NOT_FOUND: i64 = -32004;
pub const MCP_TOOL_NOT_FOUND: i64 = -32005;
pub const MCP_PROMPT_NOT_FOUND: i64 = -32006;
pub const MCP_AUTHORIZATION_FAILED: i64 = -32007;
pub const MCP_PROTOCOL_VERSION_MISMATCH: i64 = -32008;

// ---------------------------------------------------------------------------
// Specification-reserved error codes (2026-07-28).
//
// The 2026-07-28 revision partitions the JSON-RPC server-error range: -32000 to
// -32019 stays implementation-defined, with existing SDK usage grandfathered —
// which is why every code above keeps its value — and -32020 to -32099 is
// reserved for the specification itself.
// ---------------------------------------------------------------------------

/// Start (high end) of the specification-reserved sub-range.
pub const MCP_SPEC_ERROR_START: i64 = -32020;
/// End (low end) of the specification-reserved sub-range.
pub const MCP_SPEC_ERROR_END: i64 = -32099;

/// The request's headers and body disagree — e.g. `Mcp-Method` names a
/// different method than the JSON-RPC body.
pub const HEADER_MISMATCH: i64 = -32020;
/// The server requires a client capability the request did not declare.
pub const MISSING_REQUIRED_CLIENT_CAPABILITY: i64 = -32021;
/// The server does not support the protocol version the request declared.
pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;

/// Codes that positively identify a peer as speaking the 2026-era protocol.
///
/// Receiving one of these is evidence of a *modern* server even though it is a
/// rejection: a legacy server has no way to produce them, and answers an
/// unknown method with [`METHOD_NOT_FOUND`] instead.
pub const MODERN_PROTOCOL_ERRORS: &[i64] = &[
    HEADER_MISMATCH,
    MISSING_REQUIRED_CLIENT_CAPABILITY,
    UNSUPPORTED_PROTOCOL_VERSION,
];

/// Resource-not-found under 2026-07-28, which aligns it with JSON-RPC
/// `Invalid Params`. Legacy peers use [`MCP_RESOURCE_NOT_FOUND`] instead.
pub const MODERN_RESOURCE_NOT_FOUND: i64 = INVALID_PARAMS;

/// Errors that are permanent and should not be retried.
pub const NON_RETRYABLE_ERRORS: &[i64] = &[
    PARSE_ERROR,
    INVALID_REQUEST,
    METHOD_NOT_FOUND,
    INVALID_PARAMS,
    MCP_CAPABILITY_NOT_SUPPORTED,
    MCP_TOOL_NOT_FOUND,
    MCP_PROMPT_NOT_FOUND,
    MCP_AUTHORIZATION_FAILED,
    MCP_PROTOCOL_VERSION_MISMATCH,
    CONNECTION_CLOSED,
    // Protocol-level rejections. These must never be retried: unknown codes
    // default to retryable, so omitting them would make a client silently
    // re-send a request the server has already refused on protocol grounds —
    // and would stop UNSUPPORTED_PROTOCOL_VERSION from triggering re-detection.
    HEADER_MISMATCH,
    MISSING_REQUIRED_CLIENT_CAPABILITY,
    UNSUPPORTED_PROTOCOL_VERSION,
];

/// Errors that might be transient and worth retrying.
pub const RETRYABLE_ERRORS: &[i64] = &[
    INTERNAL_ERROR,
    REQUEST_TIMEOUT,
    MCP_INITIALIZATION_FAILED,
    MCP_RESOURCE_NOT_FOUND,
];

/// Get the standard description for an error code.
pub fn get_error_message(code: i64) -> String {
    match code {
        PARSE_ERROR => "Parse error: Invalid JSON was received by the server.".into(),
        INVALID_REQUEST => "Invalid Request: The JSON sent is not a valid Request object.".into(),
        METHOD_NOT_FOUND => {
            "Method not found: The method does not exist / is not available.".into()
        }
        INVALID_PARAMS => "Invalid params: Invalid method parameter(s).".into(),
        INTERNAL_ERROR => "Internal error: Internal JSON-RPC error.".into(),
        CONNECTION_CLOSED => "Connection closed".into(),
        REQUEST_TIMEOUT => "Request timeout".into(),
        MCP_INITIALIZATION_FAILED => "MCP initialization failed".into(),
        MCP_CAPABILITY_NOT_SUPPORTED => "Requested capability is not supported".into(),
        MCP_RESOURCE_NOT_FOUND => "Requested resource was not found".into(),
        MCP_TOOL_NOT_FOUND => "Requested tool was not found".into(),
        MCP_PROMPT_NOT_FOUND => "Requested prompt was not found".into(),
        MCP_AUTHORIZATION_FAILED => "Authorization failed".into(),
        MCP_PROTOCOL_VERSION_MISMATCH => "Protocol version mismatch".into(),
        HEADER_MISMATCH => "Request headers do not match the request body".into(),
        MISSING_REQUIRED_CLIENT_CAPABILITY => {
            "Request omitted a client capability the server requires".into()
        }
        UNSUPPORTED_PROTOCOL_VERSION => "Unsupported protocol version".into(),
        other => format!("Unknown error: Code {other}"),
    }
}

/// Whether an error code might be transient and worth retrying.
pub fn is_retryable_error(code: i64) -> bool {
    !NON_RETRYABLE_ERRORS.contains(&code)
}

/// Whether the code is in the reserved server error range.
pub fn is_server_error(code: i64) -> bool {
    (SERVER_ERROR_END..=SERVER_ERROR_START).contains(&code)
}

/// Whether the code is a standard JSON-RPC error.
pub fn is_standard_jsonrpc_error(code: i64) -> bool {
    matches!(
        code,
        PARSE_ERROR | INVALID_REQUEST | METHOD_NOT_FOUND | INVALID_PARAMS | INTERNAL_ERROR
    )
}

/// Whether the code is MCP-specific.
pub fn is_mcp_specific_error(code: i64) -> bool {
    matches!(
        code,
        MCP_INITIALIZATION_FAILED
            | MCP_CAPABILITY_NOT_SUPPORTED
            | MCP_RESOURCE_NOT_FOUND
            | MCP_TOOL_NOT_FOUND
            | MCP_PROMPT_NOT_FOUND
            | MCP_AUTHORIZATION_FAILED
            | MCP_PROTOCOL_VERSION_MISMATCH
            | HEADER_MISMATCH
            | MISSING_REQUIRED_CLIENT_CAPABILITY
            | UNSUPPORTED_PROTOCOL_VERSION
    )
}

/// Whether the code positively identifies a peer as speaking the 2026-era
/// protocol. See [`MODERN_PROTOCOL_ERRORS`].
pub fn is_modern_protocol_error(code: i64) -> bool {
    MODERN_PROTOCOL_ERRORS.contains(&code)
}

/// Whether the code lies in the specification-reserved sub-range
/// (`-32099..=-32020`).
pub fn is_spec_reserved_error(code: i64) -> bool {
    (MCP_SPEC_ERROR_END..=MCP_SPEC_ERROR_START).contains(&code)
}

/// The library-wide error type. Plays the role of the Python package's
/// exception hierarchy (`JSONRPCError`, `RetryableError`, `NonRetryableError`,
/// `ProtocolError`, `ValidationError`, `VersionMismatchError`, ...).
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// A JSON-RPC error response whose code marks it retryable.
    #[error("{message} (code: {code})")]
    Retryable {
        code: i64,
        message: String,
        data: Option<Value>,
    },

    /// A JSON-RPC error response whose code marks it permanent.
    #[error("{message} (code: {code})")]
    NonRetryable {
        code: i64,
        message: String,
        data: Option<Value>,
    },

    /// Error in MCP protocol handling (bad message structure, parse failures, ...).
    #[error("{message} (code: {code})")]
    Protocol {
        code: i64,
        message: String,
        data: Option<Value>,
    },

    /// Error in data validation.
    #[error("{message} (code: {code})")]
    Validation {
        code: i64,
        message: String,
        data: Option<Value>,
    },

    /// Client and server protocol versions don't match.
    #[error("Protocol version mismatch. Requested: {requested}, Supported: {supported:?}")]
    VersionMismatch {
        requested: String,
        supported: Vec<String>,
    },

    /// No matching response arrived in time.
    #[error("Request timed out after {0:?}")]
    Timeout(std::time::Duration),

    /// The request was cancelled via a cancellation token.
    #[error("Request {0} was cancelled")]
    Cancelled(String),

    /// The transport/connection failed or was closed.
    #[error("Transport error: {0}")]
    Transport(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl McpError {
    /// Build a protocol error with an explicit code.
    pub fn protocol(code: i64, message: impl Into<String>) -> Self {
        McpError::Protocol {
            code,
            message: message.into(),
            data: None,
        }
    }

    /// Build a validation error (defaults to [`INVALID_PARAMS`]).
    pub fn validation(message: impl Into<String>) -> Self {
        McpError::Validation {
            code: INVALID_PARAMS,
            message: message.into(),
            data: None,
        }
    }

    /// Classify a JSON-RPC error response into `Retryable`/`NonRetryable`,
    /// like `_process_response` in the Python package.
    pub fn from_json_rpc(code: i64, message: impl Into<String>, data: Option<Value>) -> Self {
        let message = message.into();
        if is_retryable_error(code) {
            McpError::Retryable {
                code,
                message,
                data,
            }
        } else {
            McpError::NonRetryable {
                code,
                message,
                data,
            }
        }
    }

    /// The JSON-RPC error code associated with this error, if any.
    pub fn code(&self) -> Option<i64> {
        match self {
            McpError::Retryable { code, .. }
            | McpError::NonRetryable { code, .. }
            | McpError::Protocol { code, .. }
            | McpError::Validation { code, .. } => Some(*code),
            McpError::VersionMismatch { .. } => Some(MCP_PROTOCOL_VERSION_MISMATCH),
            McpError::Timeout(_) => Some(REQUEST_TIMEOUT),
            McpError::Cancelled(_) => None,
            McpError::Transport(_) => Some(CONNECTION_CLOSED),
            McpError::Io(_) | McpError::Json(_) => None,
        }
    }

    /// Whether this error positively identifies the peer as speaking the
    /// 2026-era protocol. See [`MODERN_PROTOCOL_ERRORS`].
    pub fn is_modern_protocol_error(&self) -> bool {
        self.code().map(is_modern_protocol_error).unwrap_or(false)
    }

    /// Whether this error means a cached era decision for the peer is stale and
    /// must be re-detected.
    ///
    /// Only [`UNSUPPORTED_PROTOCOL_VERSION`] qualifies: it is the server saying
    /// the version we chose for it is wrong, which is exactly what happens when
    /// a server is upgraded (or rolled back) underneath a running client. A
    /// legacy [`McpError::VersionMismatch`] does *not* qualify — that is
    /// ordinary handshake negotiation, not an era change.
    pub fn invalidates_era(&self) -> bool {
        self.code() == Some(UNSUPPORTED_PROTOCOL_VERSION)
    }

    /// Whether this error is worth retrying.
    pub fn is_retryable(&self) -> bool {
        match self {
            McpError::Retryable { .. } | McpError::Timeout(_) => true,
            McpError::NonRetryable { .. } | McpError::Cancelled(_) => false,
            other => other.code().map(is_retryable_error).unwrap_or(false),
        }
    }

    /// Convert to a JSON-RPC error object `{code, message, data?}`.
    pub fn to_error_object(&self) -> crate::protocol::json_rpc::ErrorObject {
        let (code, data) = match self {
            McpError::Retryable { code, data, .. }
            | McpError::NonRetryable { code, data, .. }
            | McpError::Protocol { code, data, .. }
            | McpError::Validation { code, data, .. } => (*code, data.clone()),
            McpError::VersionMismatch {
                requested,
                supported,
            } => (
                MCP_PROTOCOL_VERSION_MISMATCH,
                Some(serde_json::json!({"requested": requested, "supported": supported})),
            ),
            other => (other.code().unwrap_or(INTERNAL_ERROR), None),
        };
        crate::protocol::json_rpc::ErrorObject {
            code,
            message: self.to_string(),
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryable_classification() {
        assert!(is_retryable_error(INTERNAL_ERROR));
        assert!(is_retryable_error(REQUEST_TIMEOUT));
        assert!(!is_retryable_error(METHOD_NOT_FOUND));
        assert!(!is_retryable_error(CONNECTION_CLOSED));
        // Unknown codes default to retryable, matching Python.
        assert!(is_retryable_error(-1));
    }

    #[test]
    fn error_ranges() {
        assert!(is_server_error(-32050));
        assert!(!is_server_error(-31999));
        assert!(is_standard_jsonrpc_error(PARSE_ERROR));
        assert!(is_mcp_specific_error(MCP_TOOL_NOT_FOUND));
    }

    #[test]
    fn legacy_codes_are_grandfathered_outside_the_reserved_range() {
        // The 2026-07-28 allocation policy reserves -32020..=-32099 for the
        // specification and grandfathers implementation codes in
        // -32000..=-32019. Every pre-existing code must stay in the
        // implementation half, or it would collide with a future spec code.
        for code in [
            CONNECTION_CLOSED,
            REQUEST_TIMEOUT,
            MCP_INITIALIZATION_FAILED,
            MCP_CAPABILITY_NOT_SUPPORTED,
            MCP_RESOURCE_NOT_FOUND,
            MCP_TOOL_NOT_FOUND,
            MCP_PROMPT_NOT_FOUND,
            MCP_AUTHORIZATION_FAILED,
            MCP_PROTOCOL_VERSION_MISMATCH,
        ] {
            assert!(
                !is_spec_reserved_error(code),
                "code {code} collides with the specification-reserved range"
            );
        }
    }

    #[test]
    fn modern_codes_are_reserved_and_non_retryable() {
        for code in MODERN_PROTOCOL_ERRORS {
            assert!(is_spec_reserved_error(*code));
            assert!(is_modern_protocol_error(*code));
            // Unknown codes default to retryable — these must not.
            assert!(
                !is_retryable_error(*code),
                "code {code} must not be retried"
            );
        }
        assert!(!is_modern_protocol_error(METHOD_NOT_FOUND));
        assert!(!is_modern_protocol_error(MCP_PROTOCOL_VERSION_MISMATCH));
    }

    #[test]
    fn only_unsupported_version_invalidates_the_era() {
        let unsupported = McpError::from_json_rpc(UNSUPPORTED_PROTOCOL_VERSION, "nope", None);
        assert!(unsupported.invalidates_era());
        assert!(unsupported.is_modern_protocol_error());
        assert!(!unsupported.is_retryable());

        let header = McpError::from_json_rpc(HEADER_MISMATCH, "nope", None);
        assert!(!header.invalidates_era());
        assert!(header.is_modern_protocol_error());

        // Legacy handshake negotiation is not an era change.
        let legacy = McpError::VersionMismatch {
            requested: "2025-06-18".into(),
            supported: vec!["2024-11-05".into()],
        };
        assert!(!legacy.invalidates_era());
        assert!(!legacy.is_modern_protocol_error());
    }
}
