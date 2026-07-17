//! Content type definitions, mirroring `chuk_mcp.protocol.types.content`.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::errors::McpError;

/// Who the intended customer of an object or data is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

/// Optional annotations informing the client how objects are used or displayed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Annotations {
    /// Intended audience(s) for this object or data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<Role>>,
    /// Importance from 0.0 (optional) to 1.0 (effectively required).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Text contents of a specific resource or sub-resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextResourceContents {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub text: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Binary contents (base64-encoded) of a specific resource or sub-resource.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BlobResourceContents {
    pub uri: String,
    #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub blob: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Text or binary resource contents.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResourceContents {
    Text(TextResourceContents),
    Blob(BlobResourceContents),
}

/// Any content that can appear in messages: text, images, audio, or embedded
/// resources. Tagged by the `type` field on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Content {
    #[serde(rename = "text")]
    Text {
        text: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
    #[serde(rename = "image")]
    Image {
        /// Base64-encoded image data.
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
    #[serde(rename = "audio")]
    Audio {
        /// Base64-encoded audio data.
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
    #[serde(rename = "resource")]
    EmbeddedResource {
        resource: ResourceContents,
        #[serde(skip_serializing_if = "Option::is_none")]
        annotations: Option<Annotations>,
    },
}

impl Content {
    pub fn is_text(&self) -> bool {
        matches!(self, Content::Text { .. })
    }
    pub fn is_image(&self) -> bool {
        matches!(self, Content::Image { .. })
    }
    pub fn is_audio(&self) -> bool {
        matches!(self, Content::Audio { .. })
    }
    pub fn is_embedded_resource(&self) -> bool {
        matches!(self, Content::EmbeddedResource { .. })
    }

    /// The text, if this is text content.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Content::Text { text, .. } => Some(text),
            _ => None,
        }
    }
}

/// Create a text content object.
pub fn create_text_content(text: impl Into<String>, annotations: Option<Annotations>) -> Content {
    Content::Text {
        text: text.into(),
        annotations,
    }
}

/// Create an image content object from base64 data and a MIME type.
pub fn create_image_content(
    data: impl Into<String>,
    mime_type: impl Into<String>,
    annotations: Option<Annotations>,
) -> Content {
    Content::Image {
        data: data.into(),
        mime_type: mime_type.into(),
        annotations,
    }
}

/// Create an audio content object from base64 data and a MIME type.
pub fn create_audio_content(
    data: impl Into<String>,
    mime_type: impl Into<String>,
    annotations: Option<Annotations>,
) -> Content {
    Content::Audio {
        data: data.into(),
        mime_type: mime_type.into(),
        annotations,
    }
}

/// Create an embedded text resource.
pub fn create_embedded_text_resource(
    uri: impl Into<String>,
    text: impl Into<String>,
    mime_type: Option<String>,
    annotations: Option<Annotations>,
) -> Content {
    Content::EmbeddedResource {
        resource: ResourceContents::Text(TextResourceContents {
            uri: uri.into(),
            mime_type,
            text: text.into(),
            extra: Map::new(),
        }),
        annotations,
    }
}

/// Create an annotations object.
pub fn create_annotations(audience: Option<Vec<Role>>, priority: Option<f64>) -> Annotations {
    Annotations {
        audience,
        priority,
        extra: Map::new(),
    }
}

/// Parse a JSON value into the appropriate content type.
pub fn parse_content(data: &Value) -> Result<Content, McpError> {
    serde_json::from_value(data.clone())
        .map_err(|e| McpError::validation(format!("Unknown or invalid content: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn content_wire_format() {
        let c = create_text_content("hello", None);
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"type": "text", "text": "hello"})
        );

        let c = create_image_content("QUJD", "image/png", None);
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"type": "image", "data": "QUJD", "mimeType": "image/png"})
        );
    }

    #[test]
    fn parses_embedded_resource() {
        let c = parse_content(&json!({
            "type": "resource",
            "resource": {"uri": "file:///x", "text": "body", "mimeType": "text/plain"}
        }))
        .unwrap();
        assert!(c.is_embedded_resource());
    }

    #[test]
    fn rejects_unknown_type() {
        assert!(parse_content(&json!({"type": "video", "data": "x"})).is_err());
    }
}
