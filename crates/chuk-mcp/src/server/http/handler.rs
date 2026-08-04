//! What this server does with one HTTP request.

use std::convert::Infallible;
use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use tokio::sync::mpsc;

use crate::protocol::json_rpc::{create_error_response, parse_message_str, JsonRpcMessage};
use crate::server::McpServer;

use super::response::{self, Body};
use super::sse;
use super::validate;

/// The path this server answers on. The specification puts both directions of
/// Streamable HTTP on one endpoint.
pub const MCP_PATH: &str = "/mcp";

/// The largest request body accepted, so a hostile client cannot exhaust
/// memory by never finishing one.
const MAX_BODY: usize = 10 * 1024 * 1024;

/// Handle one request.
///
/// Infallible: every failure becomes a status the client can read, because a
/// connection dropped without explanation is the least useful outcome for
/// whoever has to debug it.
pub(crate) async fn handle(
    server: Arc<McpServer>,
    options: Arc<super::HttpOptions>,
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let (parts, body) = request.into_parts();

    // Before the path, before the method, before the body is read: a request
    // from somewhere this server does not serve is refused whatever it asks
    // for. See `super::origin` for what the check is defending against.
    if let Err(reason) = super::origin::check(&parts.headers, &options.allowed_hosts) {
        tracing::warn!("refused a request: {reason}");
        return Ok(response::text_error(StatusCode::FORBIDDEN, &reason));
    }

    if parts.uri.path() != MCP_PATH {
        return Ok(response::text_error(
            StatusCode::NOT_FOUND,
            "this server serves MCP on /mcp",
        ));
    }

    Ok(match parts.method {
        Method::POST => {
            let header = |name: &str| {
                parts
                    .headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_string)
            };
            let session = header(response::SESSION_HEADER);
            let accept = header(hyper::header::ACCEPT.as_str());
            post(server, body, session, accept, &parts.headers).await
        }
        // The server-to-client stream. Accepted and held open by a server that
        // has something to push; this one answers everything on the POST that
        // asked, so it reports honestly that it has no such stream rather than
        // holding a connection open forever with nothing to say.
        Method::GET => response::status(StatusCode::METHOD_NOT_ALLOWED),
        // Ending a session is a legacy concern, and dropping the state is all
        // there is to it.
        Method::DELETE => response::accepted(),
        _ => response::status(StatusCode::METHOD_NOT_ALLOWED),
    })
}

/// Answer a POSTed JSON-RPC message.
async fn post(
    server: Arc<McpServer>,
    body: Incoming,
    session: Option<String>,
    accept: Option<String>,
    headers: &hyper::HeaderMap,
) -> Response<Body> {
    // Bounded before reading: a body that never ends would otherwise be read
    // until the process ran out of memory.
    let collected = match http_body_util::Limited::new(body, MAX_BODY).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(error) => {
            return response::text_error(
                StatusCode::BAD_REQUEST,
                &format!("could not read the request body: {error}"),
            )
        }
    };

    let text = match std::str::from_utf8(&collected) {
        Ok(text) => text,
        Err(_) => {
            return response::text_error(StatusCode::BAD_REQUEST, "the request body is not UTF-8")
        }
    };

    let message = match parse_message_str(text) {
        Ok(message) => message,
        Err(error) => {
            return response::text_error(
                StatusCode::BAD_REQUEST,
                &format!("malformed JSON-RPC message: {error}"),
            )
        }
    };

    // A 2026-era request must describe itself the same way in its headers and
    // its body. Checked before anything is dispatched, because the point of
    // the mirrored headers is that an intermediary may act on them — so a
    // request whose two halves disagree must not be executed at all.
    let modern = validate::presents_as_modern(headers);
    if modern {
        if let Some(rejection) = validate::check(&server, headers, &message) {
            tracing::warn!("refused a request: {}", rejection.message);
            return match message.id().cloned() {
                Some(id) => response::json_with_status(
                    &JsonRpcMessage::Error(create_error_response(
                        id,
                        rejection.code,
                        &rejection.message,
                        None,
                    )),
                    None,
                    StatusCode::BAD_REQUEST,
                ),
                // A notification has no id to answer against, so the status is
                // the whole of the reply.
                None => response::text_error(StatusCode::BAD_REQUEST, &rejection.message),
            };
        }
    }

    // A call to a tool that talks while it works cannot be answered with one
    // JSON body: what it says has to reach the client before the result does.
    if server.needs_stream(&message) && sse::accepts_event_stream(accept.as_deref()) {
        return stream(server, message, session);
    }

    let (answer, assigned) = server.handle_message(message, session.as_deref()).await;

    match answer {
        Some(answer) => response::json(&answer, assigned.as_deref(), modern),
        // A notification, or an answer to something this server asked. Neither
        // earns a reply, which is the difference between them and a request.
        None => response::accepted(),
    }
}

/// Answer on an event stream, handling the message in the background.
///
/// The handler owns the sending half: every notification and request it makes
/// becomes an event, the result is the last one, and dropping the sender ends
/// the stream.
fn stream(
    server: Arc<McpServer>,
    message: JsonRpcMessage,
    session: Option<String>,
) -> Response<Body> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let context = server.context_for(&message, sender.clone());

    tokio::spawn(async move {
        let (answer, _assigned) = server
            .handle_message_with(message, session.as_deref(), context)
            .await;
        if let Some(answer) = answer {
            let _ = sender.send(answer);
        }
        // Dropping the sender here is what closes the stream.
    });

    // A session is never assigned on a streamed call: only `initialize` does
    // that, and it is not a tool call.
    response::event_stream(receiver, None)
}
