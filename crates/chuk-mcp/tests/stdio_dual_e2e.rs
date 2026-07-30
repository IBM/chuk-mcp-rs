//! End-to-end tests for dual-era stdio detection, against scripted servers that
//! behave like each era — including one that dies, which must not be mistaken
//! for a legacy server.

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};

use chuk_mcp::protocol::era::{EraMode, ProtocolEra};
use chuk_mcp::protocol::messages::send_message::send_message;
use chuk_mcp::transports::stdio::StdioParameters;
use chuk_mcp::transports::stdio_dual::{stdio_client_dual, StdioDualOptions};

static COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Write `body` as a Python stdio server script and return parameters for it.
fn script_server(body: &str) -> StdioParameters {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let path = std::env::temp_dir().join(format!("mcp_dual_{}_{n}.py", std::process::id()));
    let mut f = std::fs::File::create(&path).expect("write script");
    f.write_all(PREAMBLE.as_bytes()).unwrap();
    f.write_all(body.as_bytes()).unwrap();
    f.write_all(EPILOGUE.as_bytes()).unwrap();
    StdioParameters::new("python3", [path.to_string_lossy().to_string()])
}

const PREAMBLE: &str = r#"
import sys, json

def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()

def err(i, code, message):
    send({"jsonrpc": "2.0", "id": i, "error": {"code": code, "message": message}})

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    req = json.loads(line)
    method, i = req.get("method"), req.get("id")
    params = req.get("params") or {}
    has_meta = "_meta" in params
"#;

const EPILOGUE: &str = "\n";

/// Speaks only the modern protocol.
const MODERN: &str = r#"
    if method == "server/discover":
        send({"jsonrpc": "2.0", "id": i, "result": {
            "resultType": "complete",
            "supportedVersions": ["2026-07-28"],
            "capabilities": {"tools": {}},
            "_meta": {"io.modelcontextprotocol/serverInfo":
                      {"name": "modern-demo", "version": "2.0"}},
            "instructions": "modern only"}})
    elif method == "initialize":
        err(i, -32601, "initialize is not supported")
    else:
        # Report whether the request carried per-request metadata.
        send({"jsonrpc": "2.0", "id": i,
              "result": {"resultType": "complete", "sawMeta": has_meta}})
"#;

/// Speaks only the legacy protocol: does not know `server/discover`.
const LEGACY: &str = r#"
    if method == "server/discover":
        err(i, -32601, "Method not found")
    elif method == "initialize":
        send({"jsonrpc": "2.0", "id": i, "result": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "legacy-demo", "version": "1.0"}}})
    elif method == "notifications/initialized":
        pass
    else:
        send({"jsonrpc": "2.0", "id": i,
              "result": {"sawMeta": has_meta}})
"#;

/// Modern, but rejects the version it is offered the first time.
const WRONG_VERSION: &str = r#"
    if method == "server/discover":
        if req.get("params", {}).get("_meta", {}).get(
                "io.modelcontextprotocol/protocolVersion") == "2026-07-28" and \
                not globals().get("retried"):
            globals()["retried"] = True
            send({"jsonrpc": "2.0", "id": i, "error": {
                "code": -32022, "message": "Unsupported protocol version",
                "data": {"supported": ["2026-07-28"]}}})
        else:
            send({"jsonrpc": "2.0", "id": i, "result": {
                "resultType": "complete",
                "supportedVersions": ["2026-07-28"],
                "capabilities": {}}})
    else:
        send({"jsonrpc": "2.0", "id": i, "result": {"resultType": "complete"}})
"#;

#[tokio::test]
async fn a_modern_server_is_detected_by_the_probe() {
    let conn = stdio_client_dual(script_server(MODERN), StdioDualOptions::default())
        .await
        .expect("connect");

    assert_eq!(conn.era(), ProtocolEra::Modern);
    assert_eq!(conn.profile.protocol_version, "2026-07-28");
    assert_eq!(
        conn.profile.server_info.as_ref().unwrap().name,
        "modern-demo"
    );
    assert_eq!(conn.profile.instructions.as_deref(), Some("modern only"));
    assert!(conn.transport.is_modern());

    // Every later request carries `_meta` without the caller doing anything —
    // the transport injects it, so no `send_*` helper can omit it.
    let result = send_message(&conn.read, &conn.write, "tools/list", None)
        .await
        .expect("tools/list");
    assert_eq!(result["sawMeta"], serde_json::json!(true));
}

#[tokio::test]
async fn a_legacy_server_falls_back_to_initialize() {
    let conn = stdio_client_dual(script_server(LEGACY), StdioDualOptions::default())
        .await
        .expect("connect");

    assert_eq!(conn.era(), ProtocolEra::Legacy);
    assert_eq!(conn.profile.protocol_version, "2025-06-18");
    assert_eq!(
        conn.profile.server_info.as_ref().unwrap().name,
        "legacy-demo"
    );
    assert!(!conn.transport.is_modern());

    // And crucially, no `_meta` is sent to a server that would not expect it.
    let result = send_message(&conn.read, &conn.write, "tools/list", None)
        .await
        .expect("tools/list");
    assert_eq!(result["sawMeta"], serde_json::json!(false));
}

#[tokio::test]
async fn a_rejected_version_is_renegotiated_not_fatal() {
    let conn = stdio_client_dual(script_server(WRONG_VERSION), StdioDualOptions::default())
        .await
        .expect("should recover from -32022");
    assert_eq!(conn.era(), ProtocolEra::Modern);
    assert_eq!(conn.profile.protocol_version, "2026-07-28");
}

#[tokio::test]
async fn a_server_that_dies_is_not_mistaken_for_legacy() {
    // The distinction Detection::Undetermined exists for. A crashed process
    // tells us nothing about which protocol it spoke, so falling back to
    // `initialize` would be a guess — and would report a confusing handshake
    // failure instead of the real problem.
    let conn = stdio_client_dual(
        script_server("\n    sys.exit(1)\n"),
        StdioDualOptions {
            timeout: Some(std::time::Duration::from_secs(5)),
            ..StdioDualOptions::default()
        },
    )
    .await;
    assert!(conn.is_err(), "a dead server must not resolve to an era");
}

#[tokio::test]
async fn pinning_legacy_skips_the_probe_entirely() {
    // Against a *modern* server: the pin must win, so `initialize` is attempted
    // and fails, rather than the client quietly probing and going modern.
    let result = stdio_client_dual(
        script_server(MODERN),
        StdioDualOptions {
            mode: EraMode::Legacy,
            timeout: Some(std::time::Duration::from_secs(5)),
            ..StdioDualOptions::default()
        },
    )
    .await;
    assert!(
        result.is_err(),
        "a legacy pin against a modern-only server should fail, not silently probe"
    );
}

#[tokio::test]
async fn pinning_modern_skips_the_probe_and_succeeds() {
    let conn = stdio_client_dual(
        script_server(MODERN),
        StdioDualOptions {
            mode: EraMode::Modern,
            ..StdioDualOptions::default()
        },
    )
    .await
    .expect("connect");
    assert_eq!(conn.era(), ProtocolEra::Modern);
    assert!(conn.transport.is_modern());
}
