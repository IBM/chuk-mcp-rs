//! Suggesting what a client might type next.
//!
//! `completion/complete` asks for candidate values for one argument of a
//! prompt or a resource template. A server with nothing to suggest still
//! answers: an empty list is a usable answer, "method not found" is not.

use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use serde_json::{json, Map, Value};

/// An async completion handler: what is being completed, the argument's name,
/// and the value typed so far; the candidates come back.
pub type CompletionHandler = Arc<
    dyn Fn(Value, String, String) -> Pin<Box<dyn Future<Output = Vec<String>> + Send>>
        + Send
        + Sync,
>;

/// Request and result field names.
const FIELD_REF: &str = "ref";
const FIELD_ARGUMENT: &str = "argument";
const FIELD_NAME: &str = "name";
const FIELD_VALUE: &str = "value";
const FIELD_COMPLETION: &str = "completion";
const FIELD_VALUES: &str = "values";
const FIELD_TOTAL: &str = "total";
const FIELD_HAS_MORE: &str = "hasMore";

/// The most candidates sent in one answer.
///
/// The specification caps a completion response at 100 values; anything beyond
/// that is reported through `hasMore` rather than sent.
const MAX_VALUES: usize = 100;

/// What a `completion/complete` request is asking about.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CompletionRequest {
    /// The prompt or resource template being completed.
    pub reference: Value,
    /// Which argument of it.
    pub name: String,
    /// What has been typed so far.
    pub value: String,
}

impl CompletionRequest {
    /// Read a request from its params, defaulting anything absent — a client
    /// asking vaguely gets the unfiltered list rather than an error.
    pub fn from_params(params: &Map<String, Value>) -> Self {
        let argument = params.get(FIELD_ARGUMENT);
        CompletionRequest {
            reference: params.get(FIELD_REF).cloned().unwrap_or(Value::Null),
            name: string_at(argument, FIELD_NAME),
            value: string_at(argument, FIELD_VALUE),
        }
    }
}

fn string_at(value: Option<&Value>, field: &str) -> String {
    value
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Whoever answers completion requests, if anyone does.
#[derive(Default, Clone)]
pub struct Completions {
    handler: Option<CompletionHandler>,
}

impl Completions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Supply the handler.
    pub fn set<F, Fut>(&mut self, handler: F)
    where
        F: Fn(Value, String, String) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Vec<String>> + Send + 'static,
    {
        self.handler = Some(Arc::new(move |reference, name, value| {
            Box::pin(handler(reference, name, value))
        }));
    }

    /// The `completion/complete` result for this request.
    pub async fn complete(&self, request: CompletionRequest) -> Value {
        let values = match &self.handler {
            Some(handler) => handler(request.reference, request.name, request.value).await,
            None => Vec::new(),
        };
        result_for(values)
    }
}

/// The result envelope for a set of candidates.
///
/// `total` is how many there were; `hasMore` says whether the list sent is
/// short of that, which is the only way a client can tell a full answer from a
/// truncated one.
fn result_for(values: Vec<String>) -> Value {
    let total = values.len();
    let sent: Vec<String> = values.into_iter().take(MAX_VALUES).collect();
    json!({
        FIELD_COMPLETION: {
            FIELD_VALUES: sent,
            FIELD_TOTAL: total,
            FIELD_HAS_MORE: total > MAX_VALUES,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_is_read_from_its_params() {
        let params = json!({
            FIELD_REF: {"type": "ref/prompt", FIELD_NAME: "trip"},
            FIELD_ARGUMENT: {FIELD_NAME: "city", FIELD_VALUE: "par"},
        });
        let request = CompletionRequest::from_params(params.as_object().expect("an object"));
        assert_eq!(request.name, "city");
        assert_eq!(request.value, "par");
        assert_eq!(request.reference["type"], json!("ref/prompt"));
    }

    #[test]
    fn a_request_missing_everything_still_reads() {
        let request = CompletionRequest::from_params(&Map::new());
        assert_eq!(request, CompletionRequest::default());
        assert_eq!(request.reference, Value::Null);
    }

    #[tokio::test]
    async fn without_a_handler_the_answer_is_empty_rather_than_absent() {
        let answer = Completions::new()
            .complete(CompletionRequest::default())
            .await;
        assert_eq!(answer[FIELD_COMPLETION][FIELD_VALUES], json!([]));
        assert_eq!(answer[FIELD_COMPLETION][FIELD_TOTAL], json!(0));
        assert_eq!(answer[FIELD_COMPLETION][FIELD_HAS_MORE], json!(false));
    }

    #[tokio::test]
    async fn a_handler_sees_what_was_asked_and_its_candidates_come_back() {
        let mut completions = Completions::new();
        completions.set(|_reference, name, value| async move {
            ["paris", "park", "berlin"]
                .into_iter()
                .filter(|candidate| name == "city" && candidate.starts_with(&value))
                .map(String::from)
                .collect()
        });

        let answer = completions
            .complete(CompletionRequest {
                reference: json!({"type": "ref/prompt"}),
                name: "city".into(),
                value: "par".into(),
            })
            .await;
        assert_eq!(
            answer[FIELD_COMPLETION][FIELD_VALUES],
            json!(["paris", "park"])
        );
        assert_eq!(answer[FIELD_COMPLETION][FIELD_TOTAL], json!(2));
    }

    #[test]
    fn a_short_list_is_sent_whole_and_says_so() {
        let answer = result_for(vec!["a".into(), "b".into()]);
        assert_eq!(answer[FIELD_COMPLETION][FIELD_TOTAL], json!(2));
        assert_eq!(answer[FIELD_COMPLETION][FIELD_HAS_MORE], json!(false));
    }

    #[test]
    fn a_list_past_the_cap_is_truncated_and_says_so() {
        let many: Vec<String> = (0..MAX_VALUES + 5).map(|n| n.to_string()).collect();
        let answer = result_for(many);

        assert_eq!(
            answer[FIELD_COMPLETION][FIELD_VALUES]
                .as_array()
                .expect("a list")
                .len(),
            MAX_VALUES
        );
        // The count is what there were, not what was sent — otherwise a client
        // cannot tell it is looking at a truncated list.
        assert_eq!(answer[FIELD_COMPLETION][FIELD_TOTAL], json!(MAX_VALUES + 5));
        assert_eq!(answer[FIELD_COMPLETION][FIELD_HAS_MORE], json!(true));
    }

    #[test]
    fn exactly_the_cap_is_not_truncated() {
        let exact: Vec<String> = (0..MAX_VALUES).map(|n| n.to_string()).collect();
        let answer = result_for(exact);
        assert_eq!(answer[FIELD_COMPLETION][FIELD_HAS_MORE], json!(false));
    }
}
