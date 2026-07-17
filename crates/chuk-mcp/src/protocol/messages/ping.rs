//! Ping messages, mirroring `chuk_mcp.protocol.messages.ping`.

use crate::protocol::messages::method::MessageMethod;
use crate::protocol::messages::send_message::{send_message, ReadStream, WriteStream};

/// Send a `ping` request. Returns `true` if any response arrived, `false` on
/// any failure (errors are swallowed, matching the Python behavior).
pub async fn send_ping(read_stream: &ReadStream, write_stream: &WriteStream) -> bool {
    match send_message(read_stream, write_stream, MessageMethod::PING, None).await {
        Ok(_) => true,
        Err(e) => {
            tracing::debug!("Ping failed: {e}");
            false
        }
    }
}
