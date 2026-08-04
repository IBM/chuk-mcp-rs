//! Client runner for the MCP conformance suite
//! (`@modelcontextprotocol/conformance client --command "<this> ..."`).
//!
//! The harness starts an HTTP server and invokes this binary with the server
//! URL as the final argument. We connect, run the core client flow — list, then
//! best-effort call a tool — and exit 0. The harness observes our requests and
//! judges conformance; a scenario that only exercises the handshake simply
//! ignores the tool traffic, so the tool calls are best-effort and never fail
//! the run on their own.
//!
//! The era is **detected, not chosen**. The harness tells this binary only a
//! URL — never which revision the scenario is testing — so a runner that
//! hard-coded one would fail every scenario of the other. [`chuk_mcp::connect`]
//! probes `server/discover` and drives whichever era answers, which is also
//! exactly what a real client does with an unfamiliar endpoint.

use std::sync::Arc;

use serde_json::{json, Value};

#[cfg(feature = "auth")]
use chuk_mcp::auth::{Auth, ClientIdentity, FollowRedirect};
use chuk_mcp::client::input::AcceptDefaults;
use chuk_mcp::connect::Connect;
use chuk_mcp::McpError;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    // The URL is the last argument the harness appends.
    let Some(url) = std::env::args().nth(1) else {
        eprintln!("usage: chuk-mcp-conformance-client <server-url>");
        return std::process::ExitCode::from(2);
    };

    match run(&url).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("conformance client error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(url: &str) -> Result<(), McpError> {
    // Answer elicitation from the schema's own defaults. There is no user
    // behind a conformance run, and the defaults are exactly what a client
    // "that supports defaults" would have pre-populated the form with — which
    // is what the SEP-1034 scenarios check for. Supplied before connecting so
    // a legacy handshake declares the capability; a server must not push an
    // elicitation at a client that has not said it can answer one.
    let connecting = Connect::to_url(url).input_handler(Arc::new(AcceptDefaults));

    // The suite's authorization endpoint redirects straight back with a code,
    // so the "browser" is an HTTP fetch. Asked for explicitly: the default
    // handler refuses, precisely so that a client fetching a server-named URL
    // from its own process is a decision somebody made rather than one it
    // inherited. A conformance run is exactly the case where that is right.
    #[cfg(feature = "auth")]
    let connecting = connecting.authorization(
        Auth::new()
            .handler(std::sync::Arc::new(FollowRedirect::new()))
            .identity(client_identity()),
    );

    let mut client = connecting.connect().await?;

    // Best-effort: exercise every tool the scenario offers. Errors are
    // deliberately swallowed so a discovery-only scenario still exits cleanly.
    //
    // Every tool, not just the first, and with arguments built from its own
    // schema rather than `{}` — a scenario judging what the client *sends*
    // sees nothing at all from a call whose arguments are empty, and several
    // of them exist precisely to watch a populated call go out.
    if let Ok(tools) = client.list_tools().await {
        for tool in &tools {
            // Three passes, because the header rules are three different
            // rules: values that travel as plain ASCII, values that cannot
            // and must be encoded, and values that are absent and must
            // therefore promote to no header at all. A runner that only ever
            // sent the first would leave the other two permanently untested.
            for shape in [Shape::Plain, Shape::NeedsEncoding, Shape::Absent] {
                let arguments = arguments_for(&tool.input_schema, shape);
                let _ = client.call_tool(&tool.name, arguments).await;
            }
        }
    }

    // Prompts and resources are read too: `Mcp-Name` is sourced from
    // `params.name` for one and `params.uri` for the other, so a scenario
    // checking both header shapes needs both kinds of request to happen.
    if let Ok(prompts) = client.list_prompts().await {
        for prompt in prompts.iter().take(MAX_EXERCISED) {
            let _ = client.get_prompt(&prompt.name, Some(json!({}))).await;
        }
    }
    if let Ok(resources) = client.list_resources().await {
        for resource in resources.iter().take(MAX_EXERCISED) {
            let _ = client.read_resource(&resource.uri).await;
        }
    }

    client.close().await
}

/// The client identity to authorize with.
#[cfg(feature = "auth")]
///
/// A scenario testing pre-registration hands the credentials over in
/// `MCP_CONFORMANCE_CONTEXT`, which stands in for however a real deployment
/// would configure them — a config file, an environment variable, an operator
/// pasting them into a dialog. Everything else registers dynamically or uses a
/// metadata document, so the absence of context is the ordinary case.
fn client_identity() -> ClientIdentity {
    let identity = ClientIdentity::named("chuk-mcp-conformance");

    let Ok(raw) = std::env::var("MCP_CONFORMANCE_CONTEXT") else {
        return identity;
    };
    let Ok(context) = serde_json::from_str::<Value>(&raw) else {
        return identity;
    };
    let Some(client_id) = context.get("client_id").and_then(Value::as_str) else {
        return identity;
    };

    match context.get("client_secret").and_then(Value::as_str) {
        Some(secret) => identity.with_client_secret(client_id, secret),
        None => identity.with_pre_registered(client_id),
    }
}

/// How many prompts or resources to exercise.
///
/// A bound rather than "all of them": a scenario offering a long list is
/// describing a catalogue, not asking for every entry to be fetched.
const MAX_EXERCISED: usize = 4;

/// What kind of values a call should carry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// Values that travel as plain ASCII header values.
    Plain,
    /// Values that cannot, and must arrive Base64-wrapped.
    NeedsEncoding,
    /// Optional values omitted entirely, so the headers they would have
    /// promoted to must be absent too.
    Absent,
}

/// Build call arguments from a tool's `inputSchema`.
///
/// Every declared property is filled — including optional ones, since a
/// parameter marked `x-mcp-header` is often optional and omitting it would
/// mean the header it promotes to is never sent.
///
/// [`Shape::Absent`] is the exception and the point of the third pass: every
/// optional property is sent as `null`, so a rule about what happens when a
/// promoted value is missing has a call it can actually be judged on.
fn arguments_for(schema: &Value, shape: Shape) -> Value {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return json!({});
    };
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();

    let mut arguments = serde_json::Map::new();
    for (name, property) in properties {
        let optional = !required.contains(&name.as_str());
        match shape {
            // Nothing but what the tool insists on. The optional parameters
            // are absent rather than null, which is the other way a promoted
            // header can legitimately fail to appear — and sending one a
            // scenario did not ask for looks, from the outside, exactly like
            // a client inventing headers.
            Shape::Plain | Shape::NeedsEncoding if optional => continue,
            // Present in the body but null, so the header must still be
            // omitted — the same obligation reached from the other side.
            Shape::Absent if optional => {
                arguments.insert(name.clone(), Value::Null);
                continue;
            }
            _ => {}
        }
        // A required property is always sent: omitting it would make the call
        // invalid rather than make a point about headers.
        if let Some(value) = value_for(property, shape) {
            arguments.insert(name.clone(), value);
        }
    }
    Value::Object(arguments)
}

/// A plausible value for one property, by its declared type.
///
/// `None` for a type that cannot be sensibly invented — an object or array
/// whose shape is the scenario's business, not this runner's.
fn value_for(property: &Value, shape: Shape) -> Option<Value> {
    // An enum names its own acceptable values, so guessing is never right.
    if let Some(first) = property
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return Some(first.clone());
    }
    match property.get("type").and_then(Value::as_str)? {
        // Non-ASCII, so it cannot travel as a plain header value and the
        // encoding rules have something to act on.
        "string" if shape == Shape::NeedsEncoding => Some(json!("café ☕ prüfung")),
        "string" => Some(json!("conformance")),
        "integer" => Some(json!(42)),
        "number" => Some(json!(1.5)),
        "boolean" => Some(json!(true)),
        _ => None,
    }
}
