//! Serving a server over a byte stream: newline-delimited JSON-RPC.
//!
//! The simplest transport there is, and the one a subprocess MCP server uses.
//! Nothing here is pushed to the client mid-call — a tool that wants to speak
//! while it works needs a transport with a channel back, which is what
//! [`crate::server::http`] provides.

use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};

use crate::protocol::json_rpc::parse_message_str;
use crate::protocol::types::errors::McpError;
use crate::transports::limits::read_line_bounded;

use super::McpServer;

/// What a read that ran past the limit is reported as.
const READING: &str = "inbound message";

impl McpServer {
    /// Serve over stdio: newline-delimited JSON-RPC on stdin/stdout, until
    /// stdin closes. This is how a subprocess-based MCP server runs.
    pub async fn run_stdio(&self) -> Result<(), McpError> {
        let reader = tokio::io::BufReader::new(tokio::io::stdin());
        self.serve(reader, tokio::io::stdout()).await
    }

    /// Serve newline-delimited JSON-RPC over the given reader/writer until the
    /// reader reaches EOF. [`run_stdio`](Self::run_stdio) is this with
    /// stdin/stdout.
    pub async fn serve<R, W>(&self, mut reader: R, mut writer: W) -> Result<(), McpError>
    where
        R: AsyncBufRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut session_id: Option<String> = None;

        // Bounded read: a client that never terminates a line would otherwise
        // grow this buffer until the server runs out of memory.
        while let Some(line) = read_line_bounded(&mut reader, self.max_buffer_size, READING).await?
        {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            // One bad message is not the end of the conversation: the client
            // may well send a good one next.
            let message = match parse_message_str(line) {
                Ok(message) => message,
                Err(error) => {
                    tracing::error!("Invalid message: {error}");
                    continue;
                }
            };

            let (response, new_session) = self.handle_message(message, session_id.as_deref()).await;
            if let Some(new_session) = new_session {
                session_id = Some(new_session);
            }
            if let Some(response) = response {
                let mut json = response.to_json();
                json.push('\n');
                writer.write_all(json.as_bytes()).await?;
                writer.flush().await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::messages::method::MessageMethod;
    use serde_json::{json, Value};

    fn server() -> McpServer {
        let mut server = McpServer::new("test-server", "1.0.0", None);
        server.register_tool("greet", json!({}), "Say hello", |_| async {
            Ok(json!("hello"))
        });
        server
    }

    /// Feed `input` through a server and collect what it wrote back.
    async fn exchange(server: &McpServer, input: &str) -> Vec<Value> {
        let mut output = Vec::new();
        server
            .serve(input.as_bytes(), &mut output)
            .await
            .expect("serving a finite input ends cleanly");

        String::from_utf8(output)
            .expect("responses are UTF-8")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each line is one message"))
            .collect()
    }

    #[tokio::test]
    async fn a_request_is_answered_on_the_next_line() {
        let written = exchange(
            &server(),
            &format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "id": 1, "method": MessageMethod::TOOLS_LIST})
            ),
        )
        .await;

        assert_eq!(written.len(), 1);
        assert_eq!(written[0]["result"]["tools"][0]["name"], json!("greet"));
    }

    #[tokio::test]
    async fn blank_lines_and_malformed_ones_do_not_end_the_conversation() {
        let input = format!(
            "\n   \nnot json at all\n{}\n",
            json!({"jsonrpc": "2.0", "id": 7, "method": MessageMethod::TOOLS_LIST})
        );
        let written = exchange(&server(), &input).await;

        // Only the good message was answered, and it still was.
        assert_eq!(written.len(), 1);
        assert_eq!(written[0]["id"], json!(7));
    }

    #[tokio::test]
    async fn a_notification_produces_no_line_at_all() {
        let written = exchange(
            &server(),
            &format!(
                "{}\n",
                json!({"jsonrpc": "2.0", "method": MessageMethod::NOTIFICATION_INITIALIZED})
            ),
        )
        .await;
        assert!(written.is_empty());
    }

    #[tokio::test]
    async fn several_messages_are_answered_in_order() {
        let input = format!(
            "{}\n{}\n",
            json!({"jsonrpc": "2.0", "id": 1, "method": MessageMethod::PING}),
            json!({"jsonrpc": "2.0", "id": 2, "method": MessageMethod::TOOLS_LIST}),
        );
        let written = exchange(&server(), &input).await;

        assert_eq!(written.len(), 2);
        assert_eq!(written[0]["id"], json!(1));
        assert_eq!(written[1]["id"], json!(2));
    }

    /// The cap bounds what is buffered without a newline — a client that never
    /// finishes a line. A long line that does finish is not the attack.
    #[tokio::test]
    async fn input_that_never_ends_a_line_is_refused_rather_than_buffered() {
        let server = McpServer::new("test-server", "1.0.0", None).with_max_buffer_size(32);
        let mut output = Vec::new();

        let outcome = server.serve(" ".repeat(1024).as_bytes(), &mut output).await;
        assert!(outcome.is_err(), "an unterminated line is refused");
    }

    #[tokio::test]
    async fn an_empty_stream_ends_at_once() {
        assert!(exchange(&server(), "").await.is_empty());
    }
}
