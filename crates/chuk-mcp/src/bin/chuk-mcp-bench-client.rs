//! End-to-end benchmark driver for the native Rust client.
//!
//! One of the interchangeable drivers described in `benchmarks/README.md`:
//! every driver connects to the same MCP server over stdio, performs the same
//! workload, and prints one JSON object of raw timings on stdout. Statistics
//! are computed by the orchestrator, so that all drivers are summarised by the
//! same code and differences in reporting cannot masquerade as differences in
//! speed.
//!
//! Anything this binary writes to stderr is diagnostic only.

use std::time::Instant;

use serde_json::json;

use chuk_mcp::{Connect, EraMode, McpError};

/// Name reported for this driver in the results table.
const RUNNER_NAME: &str = "rust-native";

/// Flags the driver contract defines. Kept as constants so the parser and the
/// usage message cannot drift apart.
mod flag {
    pub const SERVER_COMMAND: &str = "--server-command";
    pub const SERVER_ARG: &str = "--server-arg";
    pub const ITERATIONS: &str = "--iterations";
    pub const WARMUP: &str = "--warmup";
    pub const TOOL: &str = "--tool";
    pub const ARGUMENT_NAME: &str = "--argument-name";
    pub const ARGUMENT_VALUE: &str = "--argument-value";
}

const USAGE: &str = concat!(
    "usage: chuk-mcp-bench-client --server-command <cmd> [--server-arg <arg>]... ",
    "--iterations <n> [--warmup <n>] --tool <name> ",
    "[--argument-name <name> --argument-value <value>]"
);

/// Everything the driver contract lets the orchestrator vary.
struct Options {
    server_command: String,
    server_args: Vec<String>,
    iterations: usize,
    warmup: usize,
    tool: String,
    argument_name: Option<String>,
    argument_value: Option<String>,
}

impl Options {
    /// Parse the driver contract's flags, failing loudly on anything else: a
    /// silently ignored flag would produce a benchmark that measured the wrong
    /// workload while looking healthy.
    fn from_args() -> Result<Self, String> {
        let mut server_command = None;
        let mut server_args = Vec::new();
        let mut iterations = None;
        let mut warmup = None;
        let mut tool = None;
        let mut argument_name = None;
        let mut argument_value = None;

        let mut args = std::env::args().skip(1);
        while let Some(flag) = args.next() {
            let mut value = || {
                args.next()
                    .ok_or_else(|| format!("{flag} requires a value\n{USAGE}"))
            };
            match flag.as_str() {
                flag::SERVER_COMMAND => server_command = Some(value()?),
                flag::SERVER_ARG => server_args.push(value()?),
                flag::ITERATIONS => {
                    iterations = Some(parse_count(&value()?, flag::ITERATIONS)?);
                }
                flag::WARMUP => warmup = Some(parse_count(&value()?, flag::WARMUP)?),
                flag::TOOL => tool = Some(value()?),
                flag::ARGUMENT_NAME => argument_name = Some(value()?),
                flag::ARGUMENT_VALUE => argument_value = Some(value()?),
                other => return Err(format!("unknown flag {other}\n{USAGE}")),
            }
        }

        Ok(Options {
            server_command: server_command
                .ok_or_else(|| format!("{} is required\n{USAGE}", flag::SERVER_COMMAND))?,
            server_args,
            iterations: iterations
                .ok_or_else(|| format!("{} is required\n{USAGE}", flag::ITERATIONS))?,
            warmup: warmup.unwrap_or_default(),
            tool: tool.ok_or_else(|| format!("{} is required\n{USAGE}", flag::TOOL))?,
            argument_name,
            argument_value,
        })
    }

    /// The tool arguments, built from the optional single name/value pair the
    /// contract allows.
    fn tool_arguments(&self) -> serde_json::Value {
        match (&self.argument_name, &self.argument_value) {
            (Some(name), Some(value)) => json!({ name.as_str(): value }),
            _ => json!({}),
        }
    }
}

fn parse_count(raw: &str, flag: &str) -> Result<usize, String> {
    raw.parse()
        .map_err(|_| format!("{flag} expects a non-negative integer, got {raw:?}"))
}

#[tokio::main]
async fn main() {
    let options = match Options::from_args() {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    match run(&options).await {
        Ok(report) => println!("{report}"),
        Err(error) => {
            eprintln!("{RUNNER_NAME}: {error}");
            std::process::exit(1);
        }
    }
}

async fn run(options: &Options) -> Result<String, McpError> {
    let arguments = options.tool_arguments();

    // The handshake covers spawning the server as well as the protocol
    // exchange — the same span every other driver reports.
    //
    // The era is pinned rather than detected: the benchmark server is legacy,
    // and a detection probe would add a round trip that the Python drivers do
    // not make, turning a protocol comparison into a comparison of handshakes.
    let connect_started = Instant::now();
    let mut client = Connect::to_command(&options.server_command, options.server_args.clone())
        .era(EraMode::Legacy)
        .connect()
        .await?;
    let handshake_seconds = connect_started.elapsed().as_secs_f64();

    for _ in 0..options.warmup {
        client.call_tool(&options.tool, arguments.clone()).await?;
    }

    let mut call_seconds = Vec::with_capacity(options.iterations);
    for _ in 0..options.iterations {
        let started = Instant::now();
        client.call_tool(&options.tool, arguments.clone()).await?;
        call_seconds.push(started.elapsed().as_secs_f64());
    }

    client.close().await?;

    Ok(json!({
        "runner": RUNNER_NAME,
        "handshake_seconds": handshake_seconds,
        "call_seconds": call_seconds,
    })
    .to_string())
}
