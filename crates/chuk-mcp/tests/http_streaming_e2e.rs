//! A tool that talks while it works, over Streamable HTTP.
//!
//! The POST that carried the call stays open as an event stream: what the tool
//! says arrives before its result, and a question it asks is answered by a
//! separate POST that the server matches back to the waiting call.

use std::time::Duration;

use serde_json::{json, Value};
use tokio::net::TcpListener;

use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::server::http::{serve_on, MCP_PATH};
use chuk_mcp::server::{CallContext, LogLevel, McpServer};

/// Long enough for the spawned server to be listening.
const SETTLE: Duration = Duration::from_millis(100);

/// What a client must send to be offered a stream.
const ACCEPT_BOTH: &str = "application/json, text/event-stream";

fn server() -> McpServer {
    let mut server = McpServer::new("streaming-server", "1.0.0", None);

    server.register_tool("plain", json!({}), "Answers at once", |_| async {
        Ok(json!("done"))
    });

    server.register_interactive_tool(
        "talks",
        json!({}),
        "Speaks first",
        |_, ctx: CallContext| async move {
            ctx.log(LogLevel::Info.as_str(), json!("started"));
            ctx.progress(50.0, Some(100.0));
            Ok(json!("finished"))
        },
    );

    server.register_interactive_tool(
        "asks",
        json!({}),
        "Asks first",
        |_, ctx: CallContext| async move {
            let answer = ctx.elicit(json!({"message": "your name?"})).await?;
            Ok(json!(format!("you said {}", answer["content"]["name"])))
        },
    );

    server.register_interactive_tool(
        "asks_of_nobody",
        json!({}),
        "Asks a client that will not answer",
        |_, ctx: CallContext| async move {
            let refused = ctx.sample(json!({})).await.expect_err("nobody answers");
            Ok(json!(refused))
        },
    );

    server
}

/// Start a server and return its `/mcp` URL.
async fn spawn() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    tokio::spawn(async move {
        let _ = serve_on(
            server().with_client_timeout(Duration::from_millis(200)),
            listener,
        )
        .await;
    });
    tokio::time::sleep(SETTLE).await;
    format!("http://{address}{MCP_PATH}")
}

/// POST one message, returning the response's content type and body.
async fn post(url: &str, message: Value, accept: &str) -> (String, String) {
    let response = reqwest::Client::new()
        .post(url)
        .header(reqwest::header::ACCEPT, accept)
        .json(&message)
        .send()
        .await
        .expect("the server answers");

    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    (content_type, response.text().await.expect("a body"))
}

fn call(id: i64, name: &str) -> Value {
    json!({
        "jsonrpc": "2.0", "id": id, "method": MessageMethod::TOOLS_CALL,
        "params": {"name": name, "arguments": {}, "_meta": {"progressToken": "p1"}}
    })
}

/// Every `data:` payload in an event stream, parsed.
fn events(body: &str) -> Vec<Value> {
    body.lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .map(|data| serde_json::from_str(data).expect("each event carries one message"))
        .collect()
}

#[tokio::test]
async fn an_interactive_call_streams_what_it_says_before_its_result() {
    let url = spawn().await;
    let (content_type, body) = post(&url, call(1, "talks"), ACCEPT_BOTH).await;

    assert!(
        content_type.starts_with("text/event-stream"),
        "got {content_type}"
    );

    let events = events(&body);
    assert_eq!(events.len(), 3, "two notifications and the result: {body}");
    assert_eq!(
        events[0]["method"],
        json!(MessageMethod::NOTIFICATION_MESSAGE)
    );
    assert_eq!(
        events[1]["method"],
        json!(MessageMethod::NOTIFICATION_PROGRESS)
    );
    assert_eq!(events[1]["params"]["progressToken"], json!("p1"));

    // The result is last, and it is the answer to the call that was made.
    assert_eq!(events[2]["id"], json!(1));
    assert_eq!(events[2]["result"]["content"][0]["text"], json!("finished"));
}

#[tokio::test]
async fn an_ordinary_call_is_still_answered_with_one_json_body() {
    let url = spawn().await;
    let (content_type, body) = post(&url, call(2, "plain"), ACCEPT_BOTH).await;

    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let answer: Value = serde_json::from_str(&body).expect("one JSON message");
    assert_eq!(answer["result"]["content"][0]["text"], json!("done"));
}

/// A client that never asked for a stream gets the buffered answer, whatever
/// the tool is — anything else would be unreadable to it.
#[tokio::test]
async fn a_client_that_cannot_read_a_stream_is_not_sent_one() {
    let url = spawn().await;
    let (content_type, body) = post(&url, call(3, "talks"), "application/json").await;

    assert!(
        content_type.starts_with("application/json"),
        "{content_type}"
    );
    let answer: Value = serde_json::from_str(&body).expect("one JSON message");
    assert_eq!(answer["result"]["content"][0]["text"], json!("finished"));
}

#[tokio::test]
async fn a_question_on_the_stream_is_answered_by_a_separate_post() {
    let url = spawn().await;

    // The call streams; its answer will not arrive until the question is.
    let calling = tokio::spawn({
        let url = url.clone();
        async move { post(&url, call(4, "asks"), ACCEPT_BOTH).await }
    });

    // The elicitation is the first thing on the stream, but this side cannot
    // see it without reading the stream — so answer by id, which the server
    // generated and the tool is waiting on.
    tokio::time::sleep(SETTLE).await;
    let answered = tokio::spawn({
        let url = url.clone();
        async move {
            post(
                &url,
                json!({
                    "jsonrpc": "2.0", "id": "srv-0",
                    "result": {"action": "accept", "content": {"name": "Ada"}}
                }),
                ACCEPT_BOTH,
            )
            .await
        }
    });

    let (_, body) = calling.await.expect("the call finished");
    answered.await.expect("the answer was delivered");

    let events = events(&body);
    let question = &events[0];
    assert_eq!(
        question["method"],
        json!(MessageMethod::ELICITATION_CREATE),
        "the question comes first: {body}"
    );

    let result = events.last().expect("a result");
    assert_eq!(result["id"], json!(4));
    assert!(
        result["result"]["content"][0]["text"]
            .as_str()
            .expect("text")
            .contains("Ada"),
        "the answer reached the waiting tool: {body}"
    );
}

/// An answer earns no reply of its own — it is not a request.
#[tokio::test]
async fn an_answer_to_nothing_is_accepted_without_a_body() {
    let url = spawn().await;
    let response = reqwest::Client::new()
        .post(&url)
        .json(&json!({"jsonrpc": "2.0", "id": "srv-999", "result": {}}))
        .send()
        .await
        .expect("the server answers");

    assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
    assert!(response.text().await.expect("a body").is_empty());
}

/// A tool whose question is never answered fails rather than holding the
/// stream open for the life of the process.
#[tokio::test]
async fn a_question_nobody_answers_times_out() {
    let url = spawn().await;
    let (_, body) = post(&url, call(6, "asks_of_nobody"), ACCEPT_BOTH).await;

    let result = events(&body).last().expect("a result").clone();
    let text = result["result"]["content"][0]["text"]
        .as_str()
        .expect("text")
        .to_string();
    assert!(
        text.contains(MessageMethod::SAMPLING_CREATE_MESSAGE),
        "{text}"
    );
}

#[tokio::test]
async fn a_request_from_a_host_this_server_does_not_serve_is_refused() {
    let url = spawn().await;
    let response = reqwest::Client::new()
        .post(&url)
        .header(reqwest::header::ORIGIN, "http://evil.com")
        .json(&json!({"jsonrpc": "2.0", "id": 7, "method": MessageMethod::TOOLS_LIST}))
        .send()
        .await
        .expect("the server answers");

    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert!(response.text().await.expect("a body").contains("evil.com"));
}
