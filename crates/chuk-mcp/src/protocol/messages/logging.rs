//! Logging messages, mirroring `chuk_mcp.protocol.messages.logging`.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;

/// Syslog-style log levels accepted by `logging/setLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Notice,
    Warning,
    Error,
    Critical,
    Alert,
    Emergency,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Notice => "notice",
            LogLevel::Warning => "warning",
            LogLevel::Error => "error",
            LogLevel::Critical => "critical",
            LogLevel::Alert => "alert",
            LogLevel::Emergency => "emergency",
        }
    }
}

/// Send a `logging/setLevel` request. Returns the raw response value.
pub async fn send_logging_set_level(
    read_stream: &ReadStream,
    write_stream: &WriteStream,
    level: LogLevel,
) -> Result<Value, McpError> {
    send_message(
        read_stream,
        write_stream,
        MessageMethod::LOGGING_SET_LEVEL,
        Some(json!({"level": level.as_str()})),
    )
    .await
}
