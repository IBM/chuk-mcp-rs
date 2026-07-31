//! What a resource's body is on the wire.

use serde_json::{json, Value};

/// Resource `contents` field names.
const FIELD_URI: &str = "uri";
const FIELD_MIME_TYPE: &str = "mimeType";
const FIELD_TEXT: &str = "text";
const FIELD_BLOB: &str = "blob";

/// A resource body: text, or bytes the client will decode.
///
/// The distinction is the client's to act on, not a detail of encoding — the
/// specification carries text under `text` and everything else under `blob`,
/// and a PNG delivered as `text` is a PNG the client cannot display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceBody {
    /// Content the client can read as it stands.
    Text(String),
    /// Base64-encoded bytes.
    Blob(String),
}

impl ResourceBody {
    /// A body from a handler's string and whether that string is bytes.
    pub fn new(body: String, binary: bool) -> Self {
        if binary {
            ResourceBody::Blob(body)
        } else {
            ResourceBody::Text(body)
        }
    }

    /// Whether this body is readable text rather than encoded bytes.
    pub fn is_text(&self) -> bool {
        matches!(self, ResourceBody::Text(_))
    }

    /// The `contents` entry for this body at `uri`.
    pub fn to_contents(&self, uri: &str, mime_type: &str) -> Value {
        match self {
            ResourceBody::Text(text) => json!({
                FIELD_URI: uri, FIELD_MIME_TYPE: mime_type, FIELD_TEXT: text,
            }),
            ResourceBody::Blob(blob) => json!({
                FIELD_URI: uri, FIELD_MIME_TYPE: mime_type, FIELD_BLOB: blob,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_text_body_and_a_blob_land_in_different_fields() {
        let text = ResourceBody::Text("hello".into()).to_contents("t://x", "text/plain");
        assert_eq!(text[FIELD_TEXT], json!("hello"));
        assert!(text.get(FIELD_BLOB).is_none());
        assert_eq!(text[FIELD_URI], json!("t://x"));

        // Bytes under `text` would be bytes the client cannot decode.
        let blob = ResourceBody::Blob("aGk=".into()).to_contents("t://x", "image/png");
        assert_eq!(blob[FIELD_BLOB], json!("aGk="));
        assert!(blob.get(FIELD_TEXT).is_none());
        assert_eq!(blob[FIELD_MIME_TYPE], json!("image/png"));
    }

    #[test]
    fn a_body_reports_which_kind_it_is() {
        assert!(ResourceBody::Text(String::new()).is_text());
        assert!(!ResourceBody::Blob(String::new()).is_text());
    }

    #[test]
    fn a_body_is_built_from_whether_the_source_was_binary() {
        assert_eq!(
            ResourceBody::new("aGk=".into(), true),
            ResourceBody::Blob("aGk=".into())
        );
        assert_eq!(
            ResourceBody::new("hi".into(), false),
            ResourceBody::Text("hi".into())
        );
    }
}
