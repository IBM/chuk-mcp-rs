//! What this server does with one HTTP request.

use std::convert::Infallible;
use std::sync::Arc;

use http_body_util::BodyExt;
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};

use crate::protocol::json_rpc::parse_message_str;
use crate::server::McpServer;

use super::response::{self, Body};

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
    request: Request<Incoming>,
) -> Result<Response<Body>, Infallible> {
    let (parts, body) = request.into_parts();

    if parts.uri.path() != MCP_PATH {
        return Ok(response::text_error(
            StatusCode::NOT_FOUND,
            "this server serves MCP on /mcp",
        ));
    }

    Ok(match parts.method {
        Method::POST => {
            let session = parts
                .headers
                .get(response::SESSION_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            post(server, body, session).await
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
async fn post(server: Arc<McpServer>, body: Incoming, session: Option<String>) -> Response<Body> {
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

    let (answer, assigned) = server.handle_message(message, session.as_deref()).await;

    match answer {
        Some(answer) => response::json(&answer, assigned.as_deref()),
        // A notification earns no reply, which is the difference between it
        // and a request.
        None => response::accepted(),
    }
}
