//! `subscriptions/listen` — the server-to-client notification stream.
//!
//! The 2026-07-28 revision removed the HTTP `GET` endpoint and the
//! `resources/subscribe` RPC and put one mechanism in their place: a long-lived
//! `subscriptions/listen` request whose response stream carries change
//! notifications until the client goes away.
//!
//! Three rules shape everything here:
//!
//! * **Nothing unrequested.** The request names the notification types it wants
//!   and the server **MUST NOT** send any other. A stream opened for
//!   `promptsListChanged` that carries a `notifications/tools/list_changed` is
//!   a conformance failure, not a bonus.
//! * **The acknowledgement comes first.** Before any notification, the server
//!   sends `notifications/subscriptions/acknowledged` naming the subset it
//!   agreed to honour, so a client learns immediately which of the types it
//!   asked for it will actually get.
//! * **Everything is tagged.** Every message on the stream carries the
//!   subscription's id — the JSON-RPC id of the `subscriptions/listen` request
//!   — in `_meta`. On stdio every subscription shares one channel, so without
//!   the tag a client with two streams open could not tell them apart.
//!
//! The subscription itself is not state the protocol remembers: it lives
//! exactly as long as the stream, and a client that reconnects re-sends
//! `subscriptions/listen`.

use std::sync::Mutex;

use serde_json::{json, Map, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::protocol::json_rpc::{create_notification, create_response, JsonRpcMessage, RequestId};
use crate::protocol::messages::method::MessageMethod;
use crate::protocol::meta::SUBSCRIPTION_ID;

/// The acknowledgement a server owes as the first message on a stream.
pub const NOTIFICATION_ACKNOWLEDGED: &str = "notifications/subscriptions/acknowledged";

/// Field names of the notification filter.
const FIELD_NOTIFICATIONS: &str = "notifications";
const FIELD_TOOLS_LIST_CHANGED: &str = "toolsListChanged";
const FIELD_PROMPTS_LIST_CHANGED: &str = "promptsListChanged";
const FIELD_RESOURCES_LIST_CHANGED: &str = "resourcesListChanged";
const FIELD_RESOURCE_SUBSCRIPTIONS: &str = "resourceSubscriptions";
const FIELD_URI: &str = "uri";

/// Which notifications one subscription asked for.
///
/// Every field defaults to "not subscribed": an omitted field is not a request
/// for that type, so a filter that says nothing receives nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotificationFilter {
    pub tools_list_changed: bool,
    pub prompts_list_changed: bool,
    pub resources_list_changed: bool,
    /// Resource URIs this subscription wants `notifications/resources/updated`
    /// for. Only these — a change to any other resource is not its business.
    pub resource_subscriptions: Vec<String>,
}

impl NotificationFilter {
    /// Read a filter from a `subscriptions/listen` request's params.
    pub fn from_params(params: &Value) -> Self {
        let Some(notifications) = params.get(FIELD_NOTIFICATIONS) else {
            return NotificationFilter::default();
        };
        let flag = |name: &str| {
            notifications
                .get(name)
                .and_then(Value::as_bool)
                .unwrap_or(false)
        };
        NotificationFilter {
            tools_list_changed: flag(FIELD_TOOLS_LIST_CHANGED),
            prompts_list_changed: flag(FIELD_PROMPTS_LIST_CHANGED),
            resources_list_changed: flag(FIELD_RESOURCES_LIST_CHANGED),
            resource_subscriptions: notifications
                .get(FIELD_RESOURCE_SUBSCRIPTIONS)
                .and_then(Value::as_array)
                .map(|uris| {
                    uris.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Render as the `notifications` object of an acknowledgement.
    ///
    /// Only what was agreed to appears: a type the server will not send is
    /// omitted rather than echoed back as `false`, because the acknowledgement
    /// is a statement of what the client will get.
    pub fn to_acknowledgement(&self) -> Value {
        let mut map = Map::new();
        if self.tools_list_changed {
            map.insert(FIELD_TOOLS_LIST_CHANGED.to_string(), json!(true));
        }
        if self.prompts_list_changed {
            map.insert(FIELD_PROMPTS_LIST_CHANGED.to_string(), json!(true));
        }
        if self.resources_list_changed {
            map.insert(FIELD_RESOURCES_LIST_CHANGED.to_string(), json!(true));
        }
        if !self.resource_subscriptions.is_empty() {
            map.insert(
                FIELD_RESOURCE_SUBSCRIPTIONS.to_string(),
                json!(self.resource_subscriptions),
            );
        }
        Value::Object(map)
    }

    /// Whether this filter asked for `method`, given the resource `uri` when
    /// the notification names one.
    fn wants(&self, method: &str, uri: Option<&str>) -> bool {
        match method {
            MessageMethod::NOTIFICATION_TOOLS_LIST_CHANGED => self.tools_list_changed,
            MessageMethod::NOTIFICATION_PROMPTS_LIST_CHANGED => self.prompts_list_changed,
            MessageMethod::NOTIFICATION_RESOURCES_LIST_CHANGED => self.resources_list_changed,
            MessageMethod::NOTIFICATION_RESOURCES_UPDATED => {
                uri.is_some_and(|uri| self.resource_subscriptions.iter().any(|want| want == uri))
            }
            // Nothing else travels on this stream.
            _ => false,
        }
    }
}

/// One open `subscriptions/listen` stream.
struct Listener {
    /// The JSON-RPC id of the request that opened it, which is also the
    /// subscription id every message on it carries.
    id: RequestId,
    filter: NotificationFilter,
    outbound: UnboundedSender<JsonRpcMessage>,
}

/// Every `subscriptions/listen` stream this server currently has open.
///
/// A closed stream is not removed eagerly — there is no signal to remove it on
/// — but is pruned the next time a notification fails to send to it, which is
/// the first moment its absence is observable.
#[derive(Default)]
pub struct Listeners {
    open: Mutex<Vec<Listener>>,
}

impl Listeners {
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a subscription and send its acknowledgement.
    ///
    /// The acknowledgement goes out through the same channel before this
    /// returns, which is what guarantees it is the stream's first message.
    pub fn open(
        &self,
        id: RequestId,
        filter: NotificationFilter,
        outbound: UnboundedSender<JsonRpcMessage>,
    ) {
        let mut params = Map::new();
        params.insert(FIELD_NOTIFICATIONS.to_string(), filter.to_acknowledgement());
        params.insert("_meta".to_string(), tag(&id));

        let acknowledged = JsonRpcMessage::Notification(create_notification(
            NOTIFICATION_ACKNOWLEDGED,
            Some(Value::Object(params)),
        ));
        if outbound.send(acknowledged).is_err() {
            // The client hung up between opening the stream and being told so.
            return;
        }

        self.open.lock().expect("listeners lock").push(Listener {
            id,
            filter,
            outbound,
        });
    }

    /// How many streams are open.
    pub fn count(&self) -> usize {
        self.open.lock().expect("listeners lock").len()
    }

    /// Fan a notification out to every subscription that asked for it.
    fn fan_out(&self, method: &str, uri: Option<&str>) {
        let mut open = self.open.lock().expect("listeners lock");
        open.retain(|listener| {
            if !listener.filter.wants(method, uri) {
                // Not for this subscription — and not a reason to drop it.
                return true;
            }
            let mut params = Map::new();
            if let Some(uri) = uri {
                params.insert(FIELD_URI.to_string(), json!(uri));
            }
            params.insert("_meta".to_string(), tag(&listener.id));

            let notification = JsonRpcMessage::Notification(create_notification(
                method,
                Some(Value::Object(params)),
            ));
            // A send that fails means the client is gone, which is the only
            // way this server ever learns a stream closed.
            listener.outbound.send(notification).is_ok()
        });
    }

    /// Tell subscribers the tool list changed.
    pub fn tools_list_changed(&self) {
        self.fan_out(MessageMethod::NOTIFICATION_TOOLS_LIST_CHANGED, None);
    }

    /// Tell subscribers the prompt list changed.
    pub fn prompts_list_changed(&self) {
        self.fan_out(MessageMethod::NOTIFICATION_PROMPTS_LIST_CHANGED, None);
    }

    /// Tell subscribers the resource list changed.
    pub fn resources_list_changed(&self) {
        self.fan_out(MessageMethod::NOTIFICATION_RESOURCES_LIST_CHANGED, None);
    }

    /// Tell subscribers a resource they asked about changed.
    pub fn resource_updated(&self, uri: &str) {
        self.fan_out(MessageMethod::NOTIFICATION_RESOURCES_UPDATED, Some(uri));
    }

    /// End every open subscription gracefully.
    ///
    /// Each gets the empty response to its original request, which is how a
    /// client tells an orderly shutdown from a dropped connection: the former
    /// carries a final answer, the latter carries nothing at all.
    pub fn close_all(&self) {
        for listener in self.open.lock().expect("listeners lock").drain(..) {
            let mut result = Map::new();
            result.insert("_meta".to_string(), tag(&listener.id));
            let _ = listener
                .outbound
                .send(JsonRpcMessage::Response(create_response(
                    listener.id.clone(),
                    Some(Value::Object(result)),
                )));
        }
    }
}

/// The `_meta` block identifying which subscription a message belongs to.
fn tag(id: &RequestId) -> Value {
    json!({ SUBSCRIPTION_ID: id_value(id) })
}

/// A request id as the JSON value it was on the wire.
fn id_value(id: &RequestId) -> Value {
    match id {
        RequestId::Num(n) => json!(n),
        RequestId::Str(s) => json!(s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    fn filter_from(notifications: Value) -> NotificationFilter {
        NotificationFilter::from_params(&json!({ "notifications": notifications }))
    }

    #[test]
    fn an_absent_filter_subscribes_to_nothing() {
        let filter = NotificationFilter::from_params(&json!({}));
        assert_eq!(filter, NotificationFilter::default());
        assert!(!filter.tools_list_changed);
        assert!(filter.resource_subscriptions.is_empty());
    }

    #[test]
    fn a_filter_reads_the_fields_the_specification_names() {
        let filter = filter_from(json!({
            "toolsListChanged": true,
            "resourceSubscriptions": ["file:///a", "file:///b"],
        }));
        assert!(filter.tools_list_changed);
        assert!(!filter.prompts_list_changed);
        assert_eq!(filter.resource_subscriptions.len(), 2);
    }

    /// The acknowledgement states what the client *will* receive, so a type it
    /// did not ask for is absent rather than present-and-false.
    #[test]
    fn an_acknowledgement_names_only_what_was_agreed() {
        let ack = filter_from(json!({"promptsListChanged": true})).to_acknowledgement();
        assert_eq!(ack["promptsListChanged"], json!(true));
        assert!(ack.get("toolsListChanged").is_none());
        assert!(ack.get("resourceSubscriptions").is_none());
    }

    #[tokio::test]
    async fn opening_a_subscription_acknowledges_it_first() {
        let listeners = Listeners::new();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        listeners.open(
            RequestId::Num(7),
            filter_from(json!({"toolsListChanged": true})),
            sender,
        );

        let first = receiver.recv().await.expect("an acknowledgement");
        assert_eq!(first.method(), Some(NOTIFICATION_ACKNOWLEDGED));
        let params = first.params().unwrap();
        assert_eq!(params["_meta"][SUBSCRIPTION_ID], json!(7));
        assert_eq!(params["notifications"]["toolsListChanged"], json!(true));
    }

    #[tokio::test]
    async fn a_notification_reaches_a_subscriber_that_asked_for_it() {
        let listeners = Listeners::new();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        listeners.open(
            RequestId::Str("sub-1".into()),
            filter_from(json!({"toolsListChanged": true})),
            sender,
        );
        receiver.recv().await.expect("the acknowledgement");

        listeners.tools_list_changed();
        let sent = receiver.recv().await.expect("a notification");
        assert_eq!(
            sent.method(),
            Some(MessageMethod::NOTIFICATION_TOOLS_LIST_CHANGED)
        );
        // Every message on the stream is tagged, or a client with two streams
        // open could not tell which one this belongs to.
        assert_eq!(
            sent.params().unwrap()["_meta"][SUBSCRIPTION_ID],
            json!("sub-1")
        );
    }

    /// The rule that matters most: a server MUST NOT send a type the client
    /// did not request.
    #[tokio::test]
    async fn a_notification_is_withheld_from_a_subscriber_that_did_not_ask() {
        let listeners = Listeners::new();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        listeners.open(
            RequestId::Num(1),
            filter_from(json!({"promptsListChanged": true})),
            sender,
        );
        receiver.recv().await.expect("the acknowledgement");

        listeners.tools_list_changed();
        listeners.resources_list_changed();
        listeners.resource_updated("file:///anything");

        // Only what was asked for arrives.
        listeners.prompts_list_changed();
        let sent = receiver.recv().await.expect("a notification");
        assert_eq!(
            sent.method(),
            Some(MessageMethod::NOTIFICATION_PROMPTS_LIST_CHANGED),
            "the tools/resources notifications must never have been sent"
        );
    }

    #[tokio::test]
    async fn a_resource_update_reaches_only_the_uris_subscribed_to() {
        let listeners = Listeners::new();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        listeners.open(
            RequestId::Num(2),
            filter_from(json!({"resourceSubscriptions": ["file:///watched"]})),
            sender,
        );
        receiver.recv().await.expect("the acknowledgement");

        listeners.resource_updated("file:///ignored");
        listeners.resource_updated("file:///watched");

        let sent = receiver.recv().await.expect("a notification");
        assert_eq!(sent.params().unwrap()["uri"], json!("file:///watched"));
    }

    #[tokio::test]
    async fn a_stream_whose_client_hung_up_is_pruned() {
        let listeners = Listeners::new();
        let (sender, receiver) = mpsc::unbounded_channel();
        listeners.open(
            RequestId::Num(3),
            filter_from(json!({"toolsListChanged": true})),
            sender,
        );
        assert_eq!(listeners.count(), 1);

        drop(receiver);
        listeners.tools_list_changed();
        assert_eq!(
            listeners.count(),
            0,
            "a send that fails is the only signal the stream closed"
        );
    }

    /// A notification a subscription did not want must not prune it either.
    #[tokio::test]
    async fn an_unwanted_notification_does_not_close_a_live_stream() {
        let listeners = Listeners::new();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        listeners.open(
            RequestId::Num(4),
            filter_from(json!({"promptsListChanged": true})),
            sender,
        );
        receiver.recv().await.expect("the acknowledgement");

        listeners.tools_list_changed();
        assert_eq!(listeners.count(), 1);
    }

    #[tokio::test]
    async fn closing_sends_the_empty_result_that_marks_a_graceful_end() {
        let listeners = Listeners::new();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        listeners.open(RequestId::Num(9), NotificationFilter::default(), sender);
        receiver.recv().await.expect("the acknowledgement");

        listeners.close_all();
        let final_message = receiver.recv().await.expect("a final response");
        assert!(matches!(final_message, JsonRpcMessage::Response(_)));
        assert_eq!(
            final_message.result().unwrap()["_meta"][SUBSCRIPTION_ID],
            json!(9)
        );
        assert_eq!(listeners.count(), 0);
    }
}
