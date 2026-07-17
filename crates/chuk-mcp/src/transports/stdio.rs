//! Stdio transport: newline-delimited JSON-RPC over a subprocess,
//! mirroring `chuk_mcp.transports.stdio`.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, Mutex};

use crate::protocol::features::batching::BatchProcessor;
use crate::protocol::json_rpc::parse_message_str;
use crate::protocol::messages::send_message::{message_channel, ReadStream, WriteStream};
use crate::protocol::types::errors::McpError;
use crate::transports::Transport;

/// Environment variables inherited by default (non-Windows), matching
/// `chuk_mcp.mcp_client.host.environment`.
#[cfg(not(windows))]
pub const DEFAULT_INHERITED_ENV_VARS: &[&str] =
    &["HOME", "LOGNAME", "PATH", "SHELL", "TERM", "USER"];

#[cfg(windows)]
pub const DEFAULT_INHERITED_ENV_VARS: &[&str] = &[
    "APPDATA",
    "HOMEDRIVE",
    "HOMEPATH",
    "LOCALAPPDATA",
    "PATH",
    "PROCESSOR_ARCHITECTURE",
    "SYSTEMDRIVE",
    "SYSTEMROOT",
    "TEMP",
    "USERNAME",
    "USERPROFILE",
];

/// A safe default environment for the subprocess.
pub fn get_default_environment() -> HashMap<String, String> {
    DEFAULT_INHERITED_ENV_VARS
        .iter()
        .filter_map(|key| {
            let value = std::env::var(key).ok()?;
            // Filter exported shell function definitions (shellshock guard).
            (!value.is_empty() && !value.starts_with("()")).then(|| (key.to_string(), value))
        })
        .collect()
}

/// Parameters for stdio transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdioParameters {
    pub command: String,
    pub args: Vec<String>,
    pub env: Option<HashMap<String, String>>,
}

impl StdioParameters {
    pub fn new<I, S>(command: impl Into<String>, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        StdioParameters {
            command: command.into(),
            args: args.into_iter().map(Into::into).collect(),
            env: None,
        }
    }

    pub fn with_env(mut self, env: HashMap<String, String>) -> Self {
        self.env = Some(env);
        self
    }
}

/// Stdio transport speaking newline-delimited JSON-RPC with a subprocess.
pub struct StdioTransport {
    incoming: ReadStream,
    outgoing: WriteStream,
    child: Arc<Mutex<Option<Child>>>,
    batch_processor: Arc<std::sync::Mutex<BatchProcessor>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl StdioTransport {
    /// Spawn the server subprocess and start the reader/writer tasks.
    pub async fn start(parameters: StdioParameters) -> Result<Self, McpError> {
        if parameters.command.is_empty() {
            return Err(McpError::validation("Server command must not be empty."));
        }

        let env = parameters
            .env
            .clone()
            .unwrap_or_else(get_default_environment);

        // Suppress subprocess stderr when its LOG_LEVEL/LOGGING_LEVEL is
        // ERROR or CRITICAL, matching the Python behavior.
        let log_level = env
            .get("LOG_LEVEL")
            .or_else(|| env.get("LOGGING_LEVEL"))
            .map(|s| s.to_uppercase())
            .unwrap_or_default();
        let suppress_stderr = matches!(log_level.as_str(), "ERROR" | "CRITICAL");

        let mut child = Command::new(&parameters.command)
            .args(&parameters.args)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(if suppress_stderr {
                Stdio::null()
            } else {
                Stdio::inherit()
            })
            .process_group(0)
            .spawn()
            .map_err(|e| {
                McpError::Transport(format!("Failed to start '{}': {e}", parameters.command))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::Transport("Child stdout unavailable".into()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::Transport("Child stdin unavailable".into()))?;

        tracing::debug!(
            "Subprocess PID {:?} ({}) [stderr: {}]",
            child.id(),
            parameters.command,
            if suppress_stderr { "suppressed" } else { "pass-through" }
        );

        let (incoming_tx, incoming) = message_channel(100);
        let (outgoing, mut outgoing_rx) =
            mpsc::channel::<crate::protocol::json_rpc::JsonRpcMessage>(100);

        let batch_processor = Arc::new(std::sync::Mutex::new(BatchProcessor::default()));

        // stdout reader: parse newline-delimited JSON-RPC and route inbound.
        let reader_bp = batch_processor.clone();
        let reader = tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim();
                        if line.is_empty() {
                            continue;
                        }
                        let value: serde_json::Value = match serde_json::from_str(line) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::error!("JSON decode error: {e} [line: {:.120}]", line);
                                continue;
                            }
                        };
                        let batch_ok = reader_bp
                            .lock()
                            .expect("batch processor lock")
                            .can_process_batch(&value);
                        if !batch_ok {
                            let version = reader_bp
                                .lock()
                                .expect("batch processor lock")
                                .protocol_version
                                .clone();
                            tracing::warn!(
                                "Rejecting batch message in protocol version {version:?}"
                            );
                            continue;
                        }
                        match parse_message_str(line) {
                            Ok(msg) => {
                                // Flatten inbound batches into individual messages.
                                let messages = match msg {
                                    crate::protocol::json_rpc::JsonRpcMessage::BatchRequest(m)
                                    | crate::protocol::json_rpc::JsonRpcMessage::BatchResponse(
                                        m,
                                    ) => m,
                                    single => vec![single],
                                };
                                for m in messages {
                                    if incoming_tx.send(m).await.is_err() {
                                        return; // receiver dropped
                                    }
                                }
                            }
                            Err(e) => tracing::error!("Error processing message: {e}"),
                        }
                    }
                    Ok(None) => {
                        tracing::debug!("stdout_reader: subprocess closed stdout");
                        return;
                    }
                    Err(e) => {
                        tracing::error!("stdout_reader error: {e}");
                        return;
                    }
                }
            }
        });

        // stdin writer: serialize outbound messages as JSON lines.
        let writer = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(message) = outgoing_rx.recv().await {
                let mut json = message.to_json();
                json.push('\n');
                if let Err(e) = stdin.write_all(json.as_bytes()).await {
                    tracing::error!("stdin_writer error: {e}");
                    return;
                }
                let _ = stdin.flush().await;
                tracing::debug!(
                    "Sent: {} (id: {:?})",
                    message.method().unwrap_or("response"),
                    message.id()
                );
            }
            tracing::debug!("stdin_writer exiting; closing server stdin");
            let _ = stdin.shutdown().await;
        });

        Ok(StdioTransport {
            incoming,
            outgoing,
            child: Arc::new(Mutex::new(Some(child))),
            batch_processor,
            tasks: vec![reader, writer],
        })
    }

    /// The negotiated protocol version, if set.
    pub fn get_protocol_version(&self) -> Option<String> {
        self.batch_processor
            .lock()
            .expect("batch processor lock")
            .protocol_version
            .clone()
    }

    /// Whether JSON-RPC batching is currently enabled.
    pub fn is_batching_enabled(&self) -> bool {
        self.batch_processor
            .lock()
            .expect("batch processor lock")
            .batching_enabled
    }

    /// Terminate the subprocess: TERM, then KILL after a grace period.
    async fn terminate_process(&self) {
        let mut guard = self.child.lock().await;
        let Some(child) = guard.as_mut() else { return };

        if child.try_wait().ok().flatten().is_some() {
            return; // already exited
        }

        tracing::debug!("Terminating subprocess…");
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            // SIGTERM for graceful shutdown; kill() below sends SIGKILL.
            unsafe {
                libc_kill(pid as i32);
            }
        }

        match tokio::time::timeout(std::time::Duration::from_secs(1), child.wait()).await {
            Ok(_) => {}
            Err(_) => {
                tracing::debug!("Graceful term timed out - killing …");
                let _ = child.kill().await;
            }
        }
        *guard = None;
    }
}

#[cfg(unix)]
unsafe fn libc_kill(pid: i32) {
    // SIGTERM == 15; avoid a libc dependency for one call.
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    kill(pid, 15);
}

#[async_trait]
impl Transport for StdioTransport {
    async fn get_streams(&self) -> Result<(ReadStream, WriteStream), McpError> {
        Ok((self.incoming.clone(), self.outgoing.clone()))
    }

    fn set_protocol_version(&self, version: &str) {
        self.batch_processor
            .lock()
            .expect("batch processor lock")
            .update_protocol_version(version);
    }

    async fn close(&mut self) -> Result<(), McpError> {
        self.terminate_process().await;
        for task in self.tasks.drain(..) {
            task.abort();
        }
        Ok(())
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
        // Best-effort kill without awaiting (tokio kill_on_drop is not set;
        // start_kill sends SIGKILL immediately if still running).
        if let Ok(mut guard) = self.child.try_lock() {
            if let Some(child) = guard.as_mut() {
                let _ = child.start_kill();
            }
        }
    }
}

/// Convenience: start a stdio transport and return its streams, like the
/// Python `stdio_client` context manager. The returned transport must be kept
/// alive for the streams to stay connected.
pub async fn stdio_client(
    parameters: StdioParameters,
) -> Result<(StdioTransport, ReadStream, WriteStream), McpError> {
    let transport = StdioTransport::start(parameters).await?;
    let (read, write) = transport.get_streams().await?;
    Ok((transport, read, write))
}

/// Convenience: start a stdio transport and perform initialization, like the
/// Python `stdio_client_with_initialize`.
pub async fn stdio_client_with_initialize(
    parameters: StdioParameters,
    options: crate::protocol::messages::initialize::InitializeOptions,
) -> Result<
    (
        StdioTransport,
        ReadStream,
        WriteStream,
        crate::protocol::messages::initialize::InitializeResult,
    ),
    McpError,
> {
    let (transport, read, write) = stdio_client(parameters).await?;
    let result = crate::protocol::messages::initialize::send_initialize_with_options(
        &read, &write, options,
    )
    .await?;
    transport.set_protocol_version(&result.protocol_version);
    Ok((transport, read, write, result))
}
