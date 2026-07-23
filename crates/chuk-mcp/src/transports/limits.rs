//! Shared buffer-size limits for transport message framing.
//!
//! Every transport reads a peer's bytes incrementally and accumulates them
//! until a delimiter marks a complete message. Without an upper bound, a
//! malicious or compromised peer can simply withhold that delimiter and grow
//! the process's memory until it dies. These helpers give every transport a
//! single, configurable cap.

use tokio::io::{AsyncBufRead, AsyncBufReadExt};

use crate::protocol::types::errors::McpError;

/// Maximum bytes buffered for a single undelimited message (10 MB).
///
/// Comfortably above any normal JSON-RPC message while still bounding a
/// hostile peer. Override per-connection via the transport's parameters.
pub const DEFAULT_MAX_BUFFER_SIZE: usize = 10 * 1024 * 1024;

/// Size limits applied to a transport's inbound message framing.
///
/// Kept separate from the transport parameter structs so that adding a limit
/// never changes their shape. Construct with [`TransportLimits::default`] and
/// the `with_*` builders:
///
/// ```
/// use chuk_mcp::transports::limits::TransportLimits;
///
/// let limits = TransportLimits::default().with_max_buffer_size(1024 * 1024);
/// assert_eq!(limits.max_buffer_size, 1024 * 1024);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TransportLimits {
    /// Maximum bytes buffered for a single undelimited message (0 disables).
    pub max_buffer_size: usize,
}

impl Default for TransportLimits {
    fn default() -> Self {
        TransportLimits {
            max_buffer_size: DEFAULT_MAX_BUFFER_SIZE,
        }
    }
}

impl TransportLimits {
    /// Limits with the default cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the maximum bytes buffered for a single message (0 disables).
    pub fn with_max_buffer_size(mut self, max_buffer_size: usize) -> Self {
        self.max_buffer_size = max_buffer_size;
        self
    }
}

/// Whether `len` has outgrown `max_size`. A `max_size` of 0 disables the cap.
pub fn exceeds_limit(len: usize, max_size: usize) -> bool {
    max_size > 0 && len > max_size
}

/// The error reported when a peer sends more undelimited data than allowed.
pub fn too_large_error(len: usize, max_size: usize, context: &str) -> McpError {
    McpError::Transport(format!(
        "{context} exceeded the maximum buffered size: {len} bytes accumulated \
         without a complete message (limit: {max_size} bytes). \
         Aborting to avoid unbounded memory growth."
    ))
}

/// Read one newline-delimited line, aborting if it exceeds `max_size`.
///
/// Returns `Ok(None)` at EOF. Unlike [`AsyncBufReadExt::read_line`], the
/// accumulated line is bounded, so a peer that never sends a newline cannot
/// grow the buffer without limit. The trailing newline is not included.
pub async fn read_line_bounded<R>(
    reader: &mut R,
    max_size: usize,
    context: &str,
) -> Result<Option<String>, McpError>
where
    R: AsyncBufRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::new();

    loop {
        let (found_newline, consumed) = {
            let available = reader.fill_buf().await?;

            if available.is_empty() {
                // EOF: emit whatever trailing data we have, then stop.
                return if buf.is_empty() {
                    Ok(None)
                } else {
                    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
                };
            }

            match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    buf.extend_from_slice(&available[..pos]);
                    (true, pos + 1)
                }
                None => {
                    buf.extend_from_slice(available);
                    (false, available.len())
                }
            }
        };

        reader.consume(consumed);

        if found_newline {
            return Ok(Some(String::from_utf8_lossy(&buf).into_owned()));
        }

        if exceeds_limit(buf.len(), max_size) {
            return Err(too_large_error(buf.len(), max_size, context));
        }
    }
}

/// Read a response body as text, aborting past `max_size`.
///
/// `reqwest`'s own [`reqwest::Response::text`] applies no size limit, so the
/// body must be consumed chunk-by-chunk to enforce one.
pub async fn read_body_bounded(
    mut response: reqwest::Response,
    max_size: usize,
    context: &str,
) -> Result<String, McpError> {
    let mut buf: Vec<u8> = Vec::new();

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?
    {
        buf.extend_from_slice(&chunk);
        if exceeds_limit(buf.len(), max_size) {
            return Err(too_large_error(buf.len(), max_size, context));
        }
    }

    Ok(String::from_utf8_lossy(&buf).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limit_of_zero_disables_the_cap() {
        assert!(!exceeds_limit(usize::MAX, 0));
    }

    #[test]
    fn buffer_at_the_limit_is_allowed() {
        assert!(!exceeds_limit(100, 100));
        assert!(exceeds_limit(101, 100));
    }

    #[tokio::test]
    async fn reads_delimited_lines() {
        let mut reader: &[u8] = b"one\ntwo\n";
        assert_eq!(
            read_line_bounded(&mut reader, 100, "test").await.unwrap(),
            Some("one".to_string())
        );
        assert_eq!(
            read_line_bounded(&mut reader, 100, "test").await.unwrap(),
            Some("two".to_string())
        );
        assert_eq!(
            read_line_bounded(&mut reader, 100, "test").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn returns_trailing_data_without_newline_at_eof() {
        let mut reader: &[u8] = b"no-newline";
        assert_eq!(
            read_line_bounded(&mut reader, 100, "test").await.unwrap(),
            Some("no-newline".to_string())
        );
    }

    #[tokio::test]
    async fn rejects_a_line_that_never_ends() {
        let data = vec![b'A'; 10_000];
        let mut reader: &[u8] = &data;
        let err = read_line_bounded(&mut reader, 100, "stdio message")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("maximum buffered size"));
        assert!(err.to_string().contains("stdio message"));
    }
}
