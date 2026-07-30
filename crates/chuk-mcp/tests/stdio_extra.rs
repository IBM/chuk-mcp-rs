//! Additional stdio-transport coverage: params, env, error paths, protocol
//! version / batching, the reader's parse/batch handling, process termination,
//! and the convenience constructors.

use std::collections::HashMap;
use std::time::Duration;

use chuk_mcp::protocol::messages::initialize::InitializeOptions;
use chuk_mcp::transports::limits::TransportLimits;
use chuk_mcp::transports::stdio::{
    get_default_environment, stdio_client, stdio_client_with_initialize, StdioParameters,
    StdioTransport,
};
use chuk_mcp::transports::Transport;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_test_writer()
        .try_init();
}

fn demo() -> StdioParameters {
    StdioParameters::new(
        env!("CARGO_BIN_EXE_chuk-mcp-demo-server"),
        Vec::<String>::new(),
    )
}

#[test]
fn params_and_env() {
    let mut env = HashMap::new();
    env.insert("K".to_string(), "V".to_string());
    let p = StdioParameters::new("cmd", ["a", "b"]).with_env(env);
    assert_eq!(p.command, "cmd");
    assert_eq!(p.args, vec!["a".to_string(), "b".to_string()]);
    assert_eq!(p.env.as_ref().unwrap().get("K").unwrap(), "V");

    let default_env = get_default_environment();
    for key in default_env.keys() {
        assert!(chuk_mcp::transports::stdio::DEFAULT_INHERITED_ENV_VARS.contains(&key.as_str()));
    }
}

#[tokio::test]
async fn empty_command_errors() {
    assert!(
        StdioTransport::start(StdioParameters::new("", Vec::<String>::new()))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn bad_command_errors() {
    assert!(StdioTransport::start(StdioParameters::new(
        "/no/such/binary_zzz_qux",
        Vec::<String>::new()
    ))
    .await
    .is_err());
}

#[tokio::test]
async fn protocol_version_and_close() {
    init_tracing();
    let mut t = StdioTransport::start(demo()).await.unwrap();
    assert!(t.get_protocol_version().is_none());
    assert!(t.is_batching_enabled());

    t.set_protocol_version("2025-06-18");
    assert_eq!(t.get_protocol_version().as_deref(), Some("2025-06-18"));
    assert!(!t.is_batching_enabled());

    let (_read, _write) = t.get_streams().await.unwrap();
    t.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn reader_aborts_on_newline_free_output() {
    init_tracing();
    // A server process that streams without ever terminating a line must not
    // grow the reader's buffer without bound: the reader gives up instead.
    // The oversized run is newline-terminated and followed by a valid message:
    // an uncapped reader would skip the unparseable line and deliver the
    // message, so receiving nothing proves the cap aborted the read.
    let script = "head -c 50000 /dev/zero | tr '\\0' 'A'; \
         printf '\\n'; \
         printf '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\\n'; \
         sleep 0.4";
    let mut transport = StdioTransport::start_with_limits(
        StdioParameters::new("sh", ["-c", script]),
        TransportLimits::default().with_max_buffer_size(1000),
    )
    .await
    .unwrap();
    let (read, _write) = transport.get_streams().await.unwrap();
    let mut rx = read.lock().await;

    // The reader task exits on the oversized line, closing the channel, so the
    // trailing valid message never arrives.
    let received = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .expect("reader should stop rather than buffer indefinitely");
    assert!(received.is_none(), "expected no message, got {received:?}");

    drop(rx);
    transport.close().await.unwrap();
}

#[tokio::test]
async fn reader_handles_edge_cases() {
    init_tracing();
    // empty line, unparseable line, valid-json-but-invalid-JSONRPC, a batch of
    // requests, a batch of responses, then a single response.
    let script = "printf '\\n'; \
         printf 'not valid json\\n'; \
         printf '{\"foo\":1}\\n'; \
         printf '[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\\n'; \
         printf '[{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}]\\n'; \
         printf '{\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{}}\\n'; \
         sleep 0.4";
    let mut transport = StdioTransport::start(StdioParameters::new("sh", ["-c", script]))
        .await
        .unwrap();
    let (read, _write) = transport.get_streams().await.unwrap();
    let mut rx = read.lock().await;

    macro_rules! next {
        () => {
            tokio::time::timeout(Duration::from_secs(2), rx.recv())
                .await
                .unwrap()
                .unwrap()
        };
    }
    assert_eq!(next!().method(), Some("ping")); // batch request item
    assert!(next!().is_response()); // batch response item (id 2)
    assert!(next!().is_response()); // single response (id 3)
    drop(rx);
    transport.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn rejects_batch_when_batching_disabled() {
    init_tracing();
    let script =
        "sleep 0.3; printf '[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\\n'; sleep 0.3";
    let mut transport = StdioTransport::start(StdioParameters::new("sh", ["-c", script]))
        .await
        .unwrap();
    transport.set_protocol_version("2025-06-18"); // batching off
    let (read, _write) = transport.get_streams().await.unwrap();
    let mut rx = read.lock().await;
    // the batch is rejected -> no message is routed (either the recv times out
    // or the stream closes when the subprocess exits, but never a message).
    let res = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await;
    assert!(
        !matches!(res, Ok(Some(_))),
        "batch should have been rejected"
    );
    drop(rx);
    transport.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn close_after_process_exit() {
    init_tracing();
    // A process that exits immediately; close() finds it already gone.
    let mut transport = StdioTransport::start(StdioParameters::new("sh", ["-c", "exit 0"]))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    transport.close().await.unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn kills_stubborn_process_and_suppresses_stderr() {
    init_tracing();
    let mut env = HashMap::new();
    env.insert("LOG_LEVEL".to_string(), "ERROR".to_string());
    env.insert(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    );
    let params = StdioParameters::new("sh", ["-c", "trap '' TERM; while true; do sleep 1; done"])
        .with_env(env);
    let mut transport = StdioTransport::start(params).await.unwrap();
    let _ = transport.get_streams().await.unwrap();
    transport.close().await.unwrap();
}

#[tokio::test]
async fn drop_without_close() {
    init_tracing();
    // Dropping the transport (no close) aborts tasks and kills the child.
    {
        let transport = StdioTransport::start(demo()).await.unwrap();
        let _ = transport.get_streams().await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[tokio::test]
async fn convenience_constructors() {
    init_tracing();
    let (mut transport, _read, _write) = stdio_client(demo()).await.unwrap();
    transport.close().await.unwrap();

    let (mut transport, _read, _write, init) =
        stdio_client_with_initialize(demo(), InitializeOptions::default())
            .await
            .unwrap();
    assert_eq!(init.server_info.name, "chuk-mcp-demo");
    assert_eq!(
        transport.get_protocol_version().as_deref(),
        Some(init.protocol_version.as_str())
    );
    transport.close().await.unwrap();
}
