//! Registering and serving prompts.
//!
//! A prompt is a named, argument-taking template that a server turns into
//! messages for a model. Unlike a tool it performs nothing and returns no
//! result to act on — which is why its handler produces messages rather than a
//! value, and why `prompts/get` is the only place its arguments matter.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{json, Map, Value};

use crate::protocol::messages::prompts::{Prompt, PromptArgument, PromptMessage};
use crate::protocol::types::errors::INVALID_PARAMS;
use crate::server::context::CallContext;

/// Field names of the two results this module produces.
const FIELD_PROMPTS: &str = "prompts";
const FIELD_MESSAGES: &str = "messages";
const FIELD_DESCRIPTION: &str = "description";

/// An async prompt handler: the supplied arguments in, the messages out.
///
/// Returning `Err` describes why the prompt could not be rendered — a missing
/// argument, say — rather than producing messages that quietly omit it.
pub type PromptHandler = Arc<
    dyn Fn(
            Map<String, Value>,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<PromptMessage>, String>> + Send>>
        + Send
        + Sync,
>;

/// An async prompt handler that builds the whole result.
///
/// The 2026-07-28 revision lets `prompts/get` answer with an `input_required`
/// result — a prompt may need to ask something before it can be rendered —
/// and that is not a list of messages, so a handler that might do it returns
/// the result itself. See [`crate::protocol::mrtr`].
pub type RawPromptHandler = Arc<
    dyn Fn(
            Map<String, Value>,
            CallContext,
        ) -> Pin<Box<dyn Future<Output = Result<Value, String>> + Send>>
        + Send
        + Sync,
>;

/// How a registered prompt produces its answer.
pub(crate) enum Renderer {
    /// Messages, which this module wraps into a result.
    Messages(PromptHandler),
    /// A whole result, which the handler has already shaped.
    Raw(RawPromptHandler),
}

/// A prompt as registered, ready to be listed or rendered.
pub(crate) struct RegisteredPrompt {
    pub definition: Prompt,
    pub renderer: Renderer,
}

/// The prompts a server offers, ordered by name so `prompts/list` is
/// deterministic — clients cache it, and a set that reshuffles per request
/// defeats that for no reason.
pub(crate) type PromptRegistry = BTreeMap<String, RegisteredPrompt>;

/// Describe a prompt argument.
pub fn prompt_argument(
    name: impl Into<String>,
    description: impl Into<String>,
    required: bool,
) -> PromptArgument {
    PromptArgument {
        name: name.into(),
        description: Some(description.into()),
        required: Some(required),
        extra: Map::new(),
    }
}

/// A message in a prompt, with the text content shape.
pub fn text_message(role: impl Into<String>, text: impl Into<String>) -> PromptMessage {
    message(role, json!({"type": "text", "text": text.into()}))
}

/// A message carrying an image, as base64 with its media type.
pub fn image_message(
    role: impl Into<String>,
    data: impl Into<String>,
    mime_type: impl Into<String>,
) -> PromptMessage {
    message(
        role,
        json!({"type": "image", "data": data.into(), "mimeType": mime_type.into()}),
    )
}

/// A message carrying a resource inline, so the model sees the content rather
/// than a URI it cannot fetch.
pub fn resource_message(
    role: impl Into<String>,
    uri: impl Into<String>,
    mime_type: impl Into<String>,
    text: impl Into<String>,
) -> PromptMessage {
    message(
        role,
        json!({
            "type": "resource",
            "resource": {
                "uri": uri.into(),
                "mimeType": mime_type.into(),
                "text": text.into(),
            },
        }),
    )
}

/// A message with whatever content shape the caller has built.
pub fn message(role: impl Into<String>, content: Value) -> PromptMessage {
    PromptMessage {
        role: role.into(),
        content,
        extra: Map::new(),
    }
}

/// Add a prompt to a registry.
pub(crate) fn register<F, Fut>(
    prompts: &mut PromptRegistry,
    name: &str,
    description: &str,
    arguments: Vec<PromptArgument>,
    handler: F,
) where
    F: Fn(Map<String, Value>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<PromptMessage>, String>> + Send + 'static,
{
    let handler = Arc::new(handler);
    prompts.insert(
        name.to_string(),
        RegisteredPrompt {
            definition: Prompt {
                name: name.to_string(),
                description: Some(description.to_string()),
                // An empty argument list and no argument list say the same
                // thing; sending the empty one implies a shape that is not there.
                arguments: (!arguments.is_empty()).then_some(arguments),
                extra: Map::new(),
            },
            renderer: Renderer::Messages(Arc::new(move |args| {
                let handler = handler.clone();
                Box::pin(async move { handler(args).await })
            })),
        },
    );
    tracing::debug!("Registered prompt: {name}");
}

/// Add a prompt whose handler shapes its own result.
///
/// For a prompt that may answer with `input_required` rather than messages.
pub(crate) fn register_raw<F, Fut>(
    prompts: &mut PromptRegistry,
    name: &str,
    description: &str,
    arguments: Vec<PromptArgument>,
    handler: F,
) where
    F: Fn(Map<String, Value>, CallContext) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Value, String>> + Send + 'static,
{
    let handler = Arc::new(handler);
    prompts.insert(
        name.to_string(),
        RegisteredPrompt {
            definition: Prompt {
                name: name.to_string(),
                description: Some(description.to_string()),
                arguments: (!arguments.is_empty()).then_some(arguments),
                extra: Map::new(),
            },
            renderer: Renderer::Raw(Arc::new(move |args, context| {
                let handler = handler.clone();
                Box::pin(async move { handler(args, context).await })
            })),
        },
    );
    tracing::debug!("Registered prompt: {name}");
}

/// The `prompts/list` result.
pub(crate) fn list_result(prompts: &PromptRegistry) -> Value {
    let listed: Vec<Value> = prompts
        .values()
        .map(|prompt| serde_json::to_value(&prompt.definition).expect("a prompt serializes"))
        .collect();
    json!({ FIELD_PROMPTS: listed })
}

/// Render a prompt, or say why it could not be.
///
/// The error half is `(code, message)` so the caller can turn it into whatever
/// its transport needs.
pub(crate) async fn get_result(
    prompts: &PromptRegistry,
    name: &str,
    arguments: Map<String, Value>,
    context: &CallContext,
) -> Result<Value, (i64, String)> {
    let prompt = prompts
        .get(name)
        .ok_or_else(|| (INVALID_PARAMS, format!("Unknown prompt: {name}")))?;

    // Required arguments are checked here rather than left to the handler, so
    // every prompt reports a missing one the same way.
    if let Some(declared) = &prompt.definition.arguments {
        let missing: Vec<&str> = declared
            .iter()
            .filter(|argument| argument.required.unwrap_or(false))
            .map(|argument| argument.name.as_str())
            .filter(|name| !arguments.contains_key(*name))
            .collect();
        if !missing.is_empty() {
            return Err((
                INVALID_PARAMS,
                format!(
                    "{name} is missing required argument(s): {}",
                    missing.join(", ")
                ),
            ));
        }
    }

    let handler = match &prompt.renderer {
        // A handler that shapes its own result is taken at its word: wrapping
        // an `input_required` in `messages` would turn a question into an
        // answer that says nothing.
        Renderer::Raw(handler) => {
            return handler(arguments, context.clone())
                .await
                .map_err(|error| (INVALID_PARAMS, error))
        }
        Renderer::Messages(handler) => handler,
    };

    let messages = handler(arguments)
        .await
        .map_err(|error| (INVALID_PARAMS, error))?;

    let mut result = Map::new();
    if let Some(description) = &prompt.definition.description {
        result.insert(FIELD_DESCRIPTION.to_string(), json!(description));
    }
    result.insert(
        FIELD_MESSAGES.to_string(),
        serde_json::to_value(&messages).expect("messages serialize"),
    );
    Ok(Value::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> PromptRegistry {
        let mut prompts = PromptRegistry::new();
        prompts.insert(
            "summarise".to_string(),
            RegisteredPrompt {
                definition: Prompt {
                    name: "summarise".to_string(),
                    description: Some("Summarise a topic".to_string()),
                    arguments: Some(vec![
                        prompt_argument("topic", "What to summarise", true),
                        prompt_argument("style", "How to write it", false),
                    ]),
                    extra: Map::new(),
                },
                renderer: Renderer::Messages(Arc::new(|arguments| {
                    Box::pin(async move {
                        let topic = arguments
                            .get("topic")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string();
                        Ok(vec![text_message("user", format!("Summarise {topic}"))])
                    })
                })),
            },
        );
        prompts
    }

    fn arguments(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), json!(value)))
            .collect()
    }

    #[test]
    fn listing_reports_each_prompt_with_its_arguments() {
        let result = list_result(&registry());
        let listed = result[FIELD_PROMPTS].as_array().expect("an array");

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0]["name"], json!("summarise"));
        assert_eq!(listed[0]["arguments"][0]["name"], json!("topic"));
        assert_eq!(listed[0]["arguments"][0]["required"], json!(true));
    }

    #[tokio::test]
    async fn getting_a_prompt_renders_its_messages() {
        let result = get_result(
            &registry(),
            "summarise",
            arguments(&[("topic", "otters")]),
            &CallContext::detached(),
        )
        .await
        .expect("the prompt renders");

        assert_eq!(result[FIELD_DESCRIPTION], json!("Summarise a topic"));
        let messages = result[FIELD_MESSAGES].as_array().expect("messages");
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], json!("user"));
        assert_eq!(messages[0]["content"]["text"], json!("Summarise otters"));
    }

    #[tokio::test]
    async fn a_missing_required_argument_is_reported_by_name() {
        // Named, because "invalid params" alone leaves the caller guessing
        // which one.
        let (code, message) = get_result(
            &registry(),
            "summarise",
            Map::new(),
            &CallContext::detached(),
        )
        .await
        .expect_err("must be rejected");

        assert_eq!(code, INVALID_PARAMS);
        assert!(message.contains("topic"), "unhelpful message: {message}");
    }

    #[tokio::test]
    async fn an_optional_argument_may_be_omitted() {
        assert!(get_result(
            &registry(),
            "summarise",
            arguments(&[("topic", "otters")]),
            &CallContext::detached(),
        )
        .await
        .is_ok());
    }

    #[tokio::test]
    async fn an_unknown_prompt_is_an_error_naming_it() {
        let (code, message) = get_result(&registry(), "nope", Map::new(), &CallContext::detached())
            .await
            .expect_err("must be rejected");
        assert_eq!(code, INVALID_PARAMS);
        assert!(message.contains("nope"));
    }

    #[tokio::test]
    async fn a_handler_that_fails_explains_itself() {
        let mut prompts = PromptRegistry::new();
        prompts.insert(
            "broken".to_string(),
            RegisteredPrompt {
                definition: Prompt {
                    name: "broken".to_string(),
                    description: None,
                    arguments: None,
                    extra: Map::new(),
                },
                renderer: Renderer::Messages(Arc::new(|_| {
                    Box::pin(async { Err("the corpus is offline".to_string()) })
                })),
            },
        );

        let (_code, message) = get_result(&prompts, "broken", Map::new(), &CallContext::detached())
            .await
            .expect_err("must surface the handler's failure");
        assert!(message.contains("corpus is offline"));
    }

    #[tokio::test]
    async fn registering_describes_the_prompt_and_renders_it() {
        let mut prompts = PromptRegistry::new();
        register(
            &mut prompts,
            "greet",
            "Say hello",
            vec![prompt_argument("who", "Who to greet", true)],
            |arguments| async move {
                let who = arguments
                    .get("who")
                    .and_then(Value::as_str)
                    .unwrap_or("world");
                Ok(vec![text_message("user", format!("Hello, {who}!"))])
            },
        );

        let listed = list_result(&prompts);
        assert_eq!(listed[FIELD_PROMPTS][0]["name"], json!("greet"));
        assert_eq!(
            listed[FIELD_PROMPTS][0]["arguments"][0]["name"],
            json!("who")
        );

        let mut arguments = Map::new();
        arguments.insert("who".to_string(), json!("Ada"));
        let rendered = get_result(&prompts, "greet", arguments, &CallContext::detached())
            .await
            .expect("a prompt that renders");
        assert_eq!(
            rendered[FIELD_MESSAGES][0]["content"]["text"],
            json!("Hello, Ada!")
        );
    }

    /// An empty argument list and no argument list say the same thing, so the
    /// empty one is not sent — it would imply a shape that is not there.
    #[tokio::test]
    async fn a_prompt_with_no_arguments_declares_none() {
        let mut prompts = PromptRegistry::new();
        register(
            &mut prompts,
            "bare",
            "Nothing to fill in",
            vec![],
            |_| async { Ok(vec![text_message("user", "hello")]) },
        );

        let listed = list_result(&prompts);
        assert!(listed[FIELD_PROMPTS][0].get("arguments").is_none());
    }

    #[test]
    fn a_message_carries_whichever_content_shape_it_was_built_with() {
        let image = image_message("user", "aGk=", "image/png");
        assert_eq!(image.role, "user");
        assert_eq!(image.content["type"], json!("image"));
        assert_eq!(image.content["data"], json!("aGk="));
        assert_eq!(image.content["mimeType"], json!("image/png"));

        let embedded = resource_message("user", "t://x", "text/plain", "inside");
        assert_eq!(embedded.content["type"], json!("resource"));
        assert_eq!(embedded.content["resource"]["uri"], json!("t://x"));
        assert_eq!(embedded.content["resource"]["text"], json!("inside"));

        // And anything else the caller assembles itself.
        let custom = message("assistant", json!({"type": "audio", "data": "aGk="}));
        assert_eq!(custom.role, "assistant");
        assert_eq!(custom.content["type"], json!("audio"));
    }
}
