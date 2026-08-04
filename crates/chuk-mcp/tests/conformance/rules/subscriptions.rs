//! What `subscriptions/listen` obliges a server to do.
//!
//! The 2026-07-28 revision replaced the HTTP `GET` endpoint and
//! `resources/subscribe` with one long-lived request. Three of its rules are
//! the ones a client cannot work around if a server gets them wrong: the
//! acknowledgement must come first, every message must be tagged, and nothing
//! unrequested may ever appear.

use serde_json::json;
use tokio::sync::mpsc;

use chuk_mcp::protocol::json_rpc::{JsonRpcMessage, RequestId};
use chuk_mcp::protocol::messages::method::MessageMethod;
use chuk_mcp::protocol::meta::SUBSCRIPTION_ID;
use chuk_mcp::server::listeners::{Listeners, NotificationFilter, NOTIFICATION_ACKNOWLEDGED};

use crate::rule::{expect, expect_eq, Era, Rule, Subject, Verdict};

pub fn rules() -> Vec<Rule> {
    vec![
        Rule::new(
            "modern.server.subscription-acknowledged-first",
            Era::Modern,
            Subject::Server,
            "`notifications/subscriptions/acknowledged` is the first message on the stream",
            acknowledged_first,
        ),
        Rule::new(
            "modern.server.subscription-messages-tagged",
            Era::Modern,
            Subject::Server,
            "Every message on the stream carries `io.modelcontextprotocol/subscriptionId`",
            messages_are_tagged,
        ),
        Rule::new(
            "modern.server.subscription-filter-honoured",
            Era::Modern,
            Subject::Server,
            "A server MUST NOT send a notification type the subscription did not request",
            filter_is_honoured,
        ),
        Rule::new(
            "modern.server.subscription-closes-gracefully",
            Era::Modern,
            Subject::Server,
            "A server-ended subscription sends the empty result that marks a clean close",
            closes_gracefully,
        ),
    ]
}

/// Open a subscription and hand back the receiving end.
fn open(filter: NotificationFilter) -> (Listeners, mpsc::UnboundedReceiver<JsonRpcMessage>) {
    let listeners = Listeners::new();
    let (sender, receiver) = mpsc::unbounded_channel();
    listeners.open(RequestId::Num(7), filter, sender);
    (listeners, receiver)
}

fn filter_for(notifications: serde_json::Value) -> NotificationFilter {
    NotificationFilter::from_params(&json!({ "notifications": notifications }))
}

/// The subscription id carried by a message, if it carries one.
fn tag(message: &JsonRpcMessage) -> Option<serde_json::Value> {
    // A notification carries it in params; the closing response carries it in
    // the result. Both are "on the stream", which is what the rule is about.
    let carrier = message
        .params()
        .cloned()
        .or_else(|| message.result().cloned())?;
    carrier.get("_meta")?.get(SUBSCRIPTION_ID).cloned()
}

async fn acknowledged_first() -> Verdict {
    let (_listeners, mut receiver) = open(filter_for(json!({"toolsListChanged": true})));

    let first = receiver
        .recv()
        .await
        .ok_or("the stream carried nothing at all")?;
    expect_eq(
        "the stream's first message",
        first.method(),
        Some(NOTIFICATION_ACKNOWLEDGED),
    )?;

    // What it acknowledges is the subset agreed to, so a client learns at once
    // which of the types it asked for it will actually get.
    let notifications = first
        .params()
        .and_then(|params| params.get("notifications").cloned())
        .ok_or("the acknowledgement named no notification types")?;
    expect_eq(
        "acknowledged toolsListChanged",
        notifications.get("toolsListChanged"),
        Some(&json!(true)),
    )
}

async fn messages_are_tagged() -> Verdict {
    let (listeners, mut receiver) = open(filter_for(json!({"toolsListChanged": true})));

    let acknowledgement = receiver.recv().await.ok_or("no acknowledgement arrived")?;
    expect_eq(
        "the acknowledgement's subscriptionId",
        tag(&acknowledgement),
        Some(json!(7)),
    )?;

    listeners.tools_list_changed();
    let notification = receiver.recv().await.ok_or("no notification arrived")?;
    expect_eq(
        "the notification's subscriptionId",
        tag(&notification),
        Some(json!(7)),
    )
}

async fn filter_is_honoured() -> Verdict {
    // Subscribed to prompts only.
    let (listeners, mut receiver) = open(filter_for(json!({"promptsListChanged": true})));
    receiver.recv().await.ok_or("no acknowledgement arrived")?;

    // Everything it did not ask for.
    listeners.tools_list_changed();
    listeners.resources_list_changed();
    listeners.resource_updated("file:///anything");
    // Then the one thing it did.
    listeners.prompts_list_changed();

    let next = receiver
        .recv()
        .await
        .ok_or("the notification it subscribed to never arrived")?;
    expect_eq(
        "the first notification after three unrequested ones",
        next.method(),
        Some(MessageMethod::NOTIFICATION_PROMPTS_LIST_CHANGED),
    )
}

async fn closes_gracefully() -> Verdict {
    let (listeners, mut receiver) = open(NotificationFilter::default());
    receiver.recv().await.ok_or("no acknowledgement arrived")?;

    listeners.close_all();
    let final_message = receiver
        .recv()
        .await
        .ok_or("the subscription ended with nothing, which reads as a dropped connection")?;

    expect(
        matches!(final_message, JsonRpcMessage::Response(_)),
        "a graceful close must be the response to the original request",
    )?;
    expect_eq(
        "the closing response's subscriptionId",
        tag(&final_message),
        Some(json!(7)),
    )
}
