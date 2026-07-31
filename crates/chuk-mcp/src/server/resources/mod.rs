//! The resources a server offers: fixed URIs, templated families, and the
//! subscriptions a client holds on them.

pub mod body;
pub mod subscriptions;
pub mod template;

use std::collections::BTreeMap;
use std::pin::Pin;
use std::sync::Arc;

use futures::Future;
use serde_json::{json, Value};

pub use body::ResourceBody;
pub use subscriptions::Subscriptions;
pub use template::UriTemplate;

/// An async resource handler: returns the resource body as a string, which is
/// base64 when the resource was registered as binary.
pub type ResourceHandler =
    Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>> + Send + Sync>;

/// An async templated-resource handler: the variables the URI bound go in,
/// the body comes out.
pub type TemplateHandler = Arc<
    dyn Fn(BTreeMap<String, String>) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send>>
        + Send
        + Sync,
>;

/// Listing field names.
const FIELD_URI: &str = "uri";
const FIELD_URI_TEMPLATE: &str = "uriTemplate";
const FIELD_NAME: &str = "name";
const FIELD_DESCRIPTION: &str = "description";
const FIELD_MIME_TYPE: &str = "mimeType";
const FIELD_RESOURCES: &str = "resources";
const FIELD_RESOURCE_TEMPLATES: &str = "resourceTemplates";
const FIELD_CONTENTS: &str = "contents";

/// The media type for a resource registered without one.
const DEFAULT_MIME_TYPE: &str = "text/plain";

/// What every resource declares about itself, however it is reached.
struct Description {
    name: String,
    description: String,
    mime_type: String,
    /// Whether the handler's string is base64 bytes rather than readable text.
    binary: bool,
}

impl Description {
    fn new(name: &str, fallback: &str, description: &str, mime_type: &str, binary: bool) -> Self {
        Description {
            name: if name.is_empty() {
                fallback.to_string()
            } else {
                name.to_string()
            },
            description: description.to_string(),
            mime_type: if mime_type.is_empty() {
                DEFAULT_MIME_TYPE.to_string()
            } else {
                mime_type.to_string()
            },
            binary,
        }
    }
}

struct RegisteredResource {
    handler: ResourceHandler,
    described: Description,
}

struct RegisteredTemplate {
    template: UriTemplate,
    handler: TemplateHandler,
    described: Description,
}

/// The resources a server offers.
#[derive(Default)]
pub struct ResourceRegistry {
    fixed: BTreeMap<String, RegisteredResource>,
    templates: Vec<RegisteredTemplate>,
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a resource at one URI.
    pub fn insert<F, Fut>(
        &mut self,
        uri: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        binary: bool,
        handler: F,
    ) where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        // A resource registered without a name is known by its last path
        // segment, which is what a person would have called it anyway.
        let fallback = uri.rsplit('/').next().unwrap_or(uri).to_string();
        self.fixed.insert(
            uri.to_string(),
            RegisteredResource {
                handler: Arc::new(move || Box::pin(handler())),
                described: Description::new(name, &fallback, description, mime_type, binary),
            },
        );
        tracing::debug!("Registered resource: {uri}");
    }

    /// Register a family of resources named by a URI template.
    pub fn insert_template<F, Fut>(
        &mut self,
        uri_template: &str,
        name: &str,
        description: &str,
        mime_type: &str,
        binary: bool,
        handler: F,
    ) where
        F: Fn(BTreeMap<String, String>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<String, String>> + Send + 'static,
    {
        self.templates.push(RegisteredTemplate {
            template: UriTemplate::parse(uri_template),
            handler: Arc::new(move |bound| Box::pin(handler(bound))),
            described: Description::new(name, uri_template, description, mime_type, binary),
        });
        tracing::debug!("Registered resource template: {uri_template}");
    }

    /// The `resources/list` result.
    pub fn list_result(&self) -> Value {
        let resources: Vec<Value> = self
            .fixed
            .iter()
            .map(|(uri, resource)| {
                json!({
                    FIELD_URI: uri,
                    FIELD_NAME: resource.described.name,
                    FIELD_DESCRIPTION: resource.described.description,
                    FIELD_MIME_TYPE: resource.described.mime_type,
                })
            })
            .collect();
        json!({FIELD_RESOURCES: resources})
    }

    /// The `resources/templates/list` result.
    pub fn templates_list_result(&self) -> Value {
        let templates: Vec<Value> = self
            .templates
            .iter()
            .map(|template| {
                json!({
                    FIELD_URI_TEMPLATE: template.template.as_str(),
                    FIELD_NAME: template.described.name,
                    FIELD_DESCRIPTION: template.described.description,
                    FIELD_MIME_TYPE: template.described.mime_type,
                })
            })
            .collect();
        json!({FIELD_RESOURCE_TEMPLATES: templates})
    }

    /// Read a resource: `None` when nothing describes this URI, otherwise the
    /// `resources/read` result or the handler's failure.
    ///
    /// An exact registration is tried before any template: a URI someone
    /// registered outright is never a coincidental match for a pattern.
    pub async fn read(&self, uri: &str) -> Option<Result<Value, String>> {
        let (body, described) = match self.fixed.get(uri) {
            Some(resource) => ((resource.handler)().await, &resource.described),
            None => {
                let matched = self
                    .templates
                    .iter()
                    .find_map(|t| Some((t, t.template.match_uri(uri)?)))?;
                let (template, bound) = matched;
                ((template.handler)(bound).await, &template.described)
            }
        };

        Some(body.map(|body| {
            let body = ResourceBody::new(body, described.binary);
            json!({FIELD_CONTENTS: [body.to_contents(uri, &described.mime_type)]})
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> ResourceRegistry {
        let mut registry = ResourceRegistry::new();
        registry.insert("t://text", "", "Words", "", false, || async {
            Ok("hello".to_string())
        });
        registry.insert(
            "t://bytes",
            "bytes",
            "Pixels",
            "image/png",
            true,
            || async { Ok("aGk=".to_string()) },
        );
        registry
    }

    #[tokio::test]
    async fn a_text_resource_reads_as_text() {
        let result = registry()
            .read("t://text")
            .await
            .expect("a registered resource")
            .expect("a handler that succeeded");
        assert_eq!(result[FIELD_CONTENTS][0]["text"], json!("hello"));
        assert_eq!(result[FIELD_CONTENTS][0][FIELD_URI], json!("t://text"));
    }

    #[tokio::test]
    async fn a_binary_resource_reads_as_a_blob() {
        let result = registry()
            .read("t://bytes")
            .await
            .expect("a registered resource")
            .expect("a handler that succeeded");
        assert_eq!(result[FIELD_CONTENTS][0]["blob"], json!("aGk="));
        assert_eq!(
            result[FIELD_CONTENTS][0][FIELD_MIME_TYPE],
            json!("image/png")
        );
    }

    #[tokio::test]
    async fn an_unregistered_uri_describes_nothing() {
        assert!(registry().read("t://missing").await.is_none());
    }

    #[tokio::test]
    async fn a_handler_that_fails_says_so_rather_than_reading_as_absent() {
        let mut registry = ResourceRegistry::new();
        registry.insert("t://broken", "", "", "", false, || async {
            Err("the disk is gone".to_string())
        });

        let outcome = registry
            .read("t://broken")
            .await
            .expect("the resource exists");
        assert_eq!(outcome, Err("the disk is gone".to_string()));
    }

    #[tokio::test]
    async fn a_template_binds_and_reads() {
        let mut registry = ResourceRegistry::new();
        registry.insert_template(
            "t://item/{id}/data",
            "items",
            "A family",
            "application/json",
            false,
            |bound| async move { Ok(json!({"id": bound["id"]}).to_string()) },
        );

        let result = registry
            .read("t://item/123/data")
            .await
            .expect("the template matches")
            .expect("a handler that succeeded");
        assert!(result[FIELD_CONTENTS][0]["text"]
            .as_str()
            .expect("text")
            .contains("123"));

        assert!(registry.read("t://item/123/other").await.is_none());
    }

    #[tokio::test]
    async fn an_exact_registration_wins_over_a_template_that_also_matches() {
        let mut registry = ResourceRegistry::new();
        registry.insert_template("x://{name}", "pattern", "", "", false, |_| async {
            Ok("from the template".to_string())
        });
        registry.insert("x://exact", "exact", "", "", false, || async {
            Ok("from the registration".to_string())
        });

        let result = registry
            .read("x://exact")
            .await
            .expect("registered")
            .expect("succeeded");
        assert_eq!(
            result[FIELD_CONTENTS][0]["text"],
            json!("from the registration")
        );
    }

    #[test]
    fn listing_reports_what_was_registered() {
        let listed = registry().list_result();
        let entries = listed[FIELD_RESOURCES].as_array().expect("a list");
        assert_eq!(entries.len(), 2);

        // Registered without a name, so known by its last path segment; and
        // without a media type, so text by default.
        let text = entries
            .iter()
            .find(|entry| entry[FIELD_URI] == json!("t://text"))
            .expect("the text resource");
        assert_eq!(text[FIELD_NAME], json!("text"));
        assert_eq!(text[FIELD_MIME_TYPE], json!(DEFAULT_MIME_TYPE));
    }

    #[test]
    fn templates_are_listed_with_the_pattern_as_written() {
        let mut registry = ResourceRegistry::new();
        registry.insert_template("t://item/{id}", "", "A family", "", false, |_| async {
            Ok(String::new())
        });

        let listed = registry.templates_list_result();
        let entry = &listed[FIELD_RESOURCE_TEMPLATES][0];
        assert_eq!(entry[FIELD_URI_TEMPLATE], json!("t://item/{id}"));
        // Unnamed, so known by the pattern itself.
        assert_eq!(entry[FIELD_NAME], json!("t://item/{id}"));
    }

    #[test]
    fn a_registry_with_nothing_in_it_lists_nothing() {
        let empty = ResourceRegistry::new();
        assert_eq!(empty.list_result()[FIELD_RESOURCES], json!([]));
        assert_eq!(
            empty.templates_list_result()[FIELD_RESOURCE_TEMPLATES],
            json!([])
        );
    }
}
