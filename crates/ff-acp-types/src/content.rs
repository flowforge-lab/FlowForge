//! Content blocks — the displayable payloads that travel in prompts, tool call
//! results, and `session/update` notifications.
//!
//! ACP content blocks are compatible with MCP's content blocks: an agent can
//! forward MCP tool output without transformation. The wire discriminator is the
//! `type` member, so all variants derive with `#[serde(tag = "type")]`.

use serde::{Deserialize, Serialize};

use crate::rpc::Meta;

/// A content block. Tagged by its `type` member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentBlock {
    /// Text content, plain or Markdown. All agents MUST support text blocks in
    /// prompts; clients SHOULD render them as Markdown.
    Text(TextContent),
    /// An image for visual context. Requires the `image` prompt capability.
    Image(ImageContent),
    /// Audio data. Requires the `audio` prompt capability.
    Audio(AudioContent),
    /// A link to a resource the agent can read. All agents MUST support resource
    /// links in prompts.
    #[serde(rename = "resource_link")]
    ResourceLink(ResourceLink),
    /// Resource contents embedded directly in the message. Requires the
    /// `embeddedContext` prompt capability.
    Resource(EmbeddedResource),
}

/// Optional annotations that help the client decide how to display or route
/// content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Annotations {
    /// Intended recipients for this content (user and/or assistant).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audience: Option<Vec<Role>>,
    /// When the underlying resource was last modified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_modified: Option<String>,
    /// Relative importance when clients choose what to surface.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The sender or recipient of messages and data in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Assistant,
    User,
}

/// Text content provided to or from an LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// An image provided to or from an LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// Base64-encoded media payload.
    pub data: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Audio provided to or from an LLM.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioContent {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    /// Base64-encoded media payload.
    pub data: String,
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A resource the agent is capable of reading, linked rather than embedded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLink {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// The payload of an embedded resource: text or binary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum EmbeddedResourceResource {
    Text(TextResourceContents),
    Blob(BlobResourceContents),
}

/// Text resource contents embedded directly in the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextResourceContents {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub text: String,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// Binary resource contents embedded directly in the message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobResourceContents {
    /// Base64-encoded bytes.
    pub blob: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    pub uri: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

/// A resource embedded into a prompt or tool call result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedResource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Annotations>,
    pub resource: EmbeddedResourceResource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub _meta: Option<Meta>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_content_round_trip() {
        let block = ContentBlock::Text(TextContent {
            annotations: None,
            text: "Hello, world!".into(),
            _meta: None,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "text");
        assert_eq!(json["text"], "Hello, world!");

        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn test_image_content_round_trip() {
        let block = ContentBlock::Image(ImageContent {
            annotations: None,
            data: "iVBORw0KGgoAAAANSUhEUg==".into(),
            mime_type: "image/png".into(),
            uri: None,
            _meta: None,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "image");
        assert_eq!(json["data"], "iVBORw0KGgoAAAANSUhEUg==");
        assert_eq!(json["mimeType"], "image/png");

        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn test_audio_content_round_trip() {
        let block = ContentBlock::Audio(AudioContent {
            annotations: None,
            data: "base64audio==".into(),
            mime_type: "audio/wav".into(),
            _meta: None,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "audio");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn test_resource_link_round_trip() {
        let block = ContentBlock::ResourceLink(ResourceLink {
            annotations: None,
            description: None,
            mime_type: None,
            name: "README".into(),
            size: None,
            title: Some("README.md".into()),
            uri: "file:///workspace/README.md".into(),
            _meta: None,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "resource_link");
        assert_eq!(json["name"], "README");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn test_embedded_text_resource_round_trip() {
        let block = ContentBlock::Resource(EmbeddedResource {
            annotations: None,
            resource: EmbeddedResourceResource::Text(TextResourceContents {
                mime_type: Some("text/plain".into()),
                text: "file content".into(),
                uri: "file:///workspace/file.txt".into(),
                _meta: None,
            }),
            _meta: None,
        });
        let json = serde_json::to_value(&block).unwrap();
        assert_eq!(json["type"], "resource");
        assert_eq!(json["resource"]["text"], "file content");
        let back: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(back, block);
    }

    #[test]
    fn test_unknown_fields_tolerated_on_text_content() {
        let json = serde_json::json!({
            "type": "text",
            "text": "hello",
            "unknownField": "should not cause error"
        });
        let block: ContentBlock = serde_json::from_value(json).unwrap();
        assert_eq!(
            block,
            ContentBlock::Text(TextContent {
                annotations: None,
                text: "hello".into(),
                _meta: None,
            })
        );
    }

    #[test]
    fn test_role_round_trip() {
        let role = Role::Assistant;
        let json = serde_json::to_value(role).unwrap();
        assert_eq!(json, "assistant");
        let back: Role = serde_json::from_value(json).unwrap();
        assert_eq!(back, Role::Assistant);
    }

    #[test]
    fn test_annotations_round_trip() {
        let ann = Annotations {
            audience: Some(vec![Role::User, Role::Assistant]),
            last_modified: Some("2026-01-01T00:00:00Z".into()),
            priority: Some(0.5),
            _meta: None,
        };
        let json = serde_json::to_value(&ann).unwrap();
        assert_eq!(json["audience"], serde_json::json!(["user", "assistant"]));
        let back: Annotations = serde_json::from_value(json).unwrap();
        assert_eq!(back, ann);
    }
}
