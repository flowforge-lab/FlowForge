//! Integration tests that verify our types agree with real ACP wire payloads.
//!
//! Unlike the unit tests which round-trip through our own serializer, these tests
//! parse verbatim JSON strings derived from the ACP v1 schema — so they catch
//! mismatches between our serde attributes and the actual wire shape.
//!
//! Each fixture was built from the schema's required/optional field lists at
//! `schema/v1/schema.json`.

use ff_acp_types::agent::*;
use ff_acp_types::client::*;
use ff_acp_types::content::*;

// ---------------------------------------------------------------------------
// Positive tests: verbatim payloads from the schema
// ---------------------------------------------------------------------------

/// `WriteTextFileRequest` (schema line 266–290): required fields sessionId,
/// path, content. This is the exact shape an ACP client would send.
#[test]
fn write_text_file_request_from_schema_fixture() {
    let payload = serde_json::from_str::<WriteTextFileRequest>(
        r#"{"sessionId":"sess_01","path":"/workspace/main.rs","content":"fn main() {}"}"#,
    )
    .expect("valid WriteTextFileRequest should deserialize");
    assert_eq!(payload.session_id, "sess_01");
    assert_eq!(payload.path, "/workspace/main.rs");
    assert_eq!(payload.content, "fn main() {}");
}

/// `WriteTextFileResponse` — empty result object (no required fields).
#[test]
fn write_text_file_response_from_schema_fixture() {
    let payload = serde_json::from_str::<WriteTextFileResponse>(r#"{}"#)
        .expect("valid WriteTextFileResponse should deserialize");
    assert!(payload._meta.is_none());
}

/// `ReadTextFileRequest` (schema line 292–340): required fields sessionId, path.
/// line and limit are optional.
#[test]
fn read_text_file_request_from_schema_fixture() {
    let payload = serde_json::from_str::<ReadTextFileRequest>(
        r#"{"sessionId":"sess_01","path":"/workspace/Cargo.toml","line":1,"limit":50}"#,
    )
    .expect("valid ReadTextFileRequest should deserialize");
    assert_eq!(payload.session_id, "sess_01");
    assert_eq!(payload.path, "/workspace/Cargo.toml");
    assert_eq!(payload.line, Some(1));
    assert_eq!(payload.limit, Some(50));
}

/// `ReadTextFileResponse` — required field content.
#[test]
fn read_text_file_response_from_schema_fixture() {
    let payload = serde_json::from_str::<ReadTextFileResponse>(
        r#"{"content":"[package]\nname = \"my-project\""}"#,
    )
    .expect("valid ReadTextFileResponse should deserialize");
    assert!(payload.content.starts_with("[package]"));
}

/// `RequestPermissionRequest` (schema line 378–405): required fields
/// sessionId, toolCall, options.
#[test]
fn request_permission_request_from_schema_fixture() {
    let payload = serde_json::from_str::<RequestPermissionRequest>(
        r#"{
            "sessionId": "sess_01",
            "toolCall": {
                "toolCallId": "tc_1",
                "title": "Edit file",
                "status": "pending"
            },
            "options": [
                {
                    "optionId": "opt_allow_once",
                    "name": "Allow once",
                    "kind": "allow_once"
                }
            ]
        }"#,
    )
    .expect("valid RequestPermissionRequest should deserialize");
    assert_eq!(payload.session_id, "sess_01");
    assert_eq!(payload.tool_call.tool_call_id, "tc_1");
    assert_eq!(payload.options.len(), 1);
    assert_eq!(payload.options[0].option_id, "opt_allow_once");
}

/// `TextContent` via `ContentBlock` — required field text (schema line 492–508).
#[test]
fn content_block_text_from_schema_fixture() {
    let block = serde_json::from_str::<ContentBlock>(r#"{"type":"text","text":"Hello, world!"}"#)
        .expect("valid text ContentBlock should deserialize");
    match block {
        ContentBlock::Text(t) => assert_eq!(t.text, "Hello, world!"),
        other => panic!("expected Text, got {other:?}"),
    }
}

/// `ContentBlock` image — required fields data, mimeType.
#[test]
fn content_block_image_from_schema_fixture() {
    let block = serde_json::from_str::<ContentBlock>(
        r#"{"type":"image","data":"iVBORw0KGgo=","mimeType":"image/png"}"#,
    )
    .expect("valid image ContentBlock should deserialize");
    match block {
        ContentBlock::Image(i) => {
            assert_eq!(i.data, "iVBORw0KGgo=");
            assert_eq!(i.mime_type, "image/png");
        }
        other => panic!("expected Image, got {other:?}"),
    }
}

/// `InitializeRequest` — required field protocolVersion.
#[test]
fn initialize_request_from_schema_fixture() {
    let payload = serde_json::from_str::<InitializeRequest>(r#"{"protocolVersion":1}"#)
        .expect("valid InitializeRequest should deserialize");
    assert_eq!(payload.protocol_version, 1);
}

/// `InitializeResponse` — required field protocolVersion.
#[test]
fn initialize_response_from_schema_fixture() {
    let payload = serde_json::from_str::<InitializeResponse>(r#"{"protocolVersion":1}"#)
        .expect("valid InitializeResponse should deserialize");
    assert_eq!(payload.protocol_version, 1);
    assert!(payload.auth_methods.is_empty());
}

/// `PromptRequest` — required fields sessionId, prompt.
#[test]
fn prompt_request_from_schema_fixture() {
    let payload = serde_json::from_str::<PromptRequest>(
        r#"{
            "sessionId": "sess_01",
            "prompt": [
                {
                    "role": "user",
                    "content": [
                        {"type": "text", "text": "Hello"}
                    ]
                }
            ]
        }"#,
    )
    .expect("valid PromptRequest should deserialize");
    assert_eq!(payload.session_id, "sess_01");
    assert_eq!(payload.prompt.len(), 1);
}

/// `TextResourceContents` via `EmbeddedResource` — required fields text, uri.
#[test]
fn embedded_resource_text_from_schema_fixture() {
    let block = serde_json::from_str::<ContentBlock>(
        r#"{
            "type": "resource",
            "resource": {
                "text": "file content",
                "uri": "file:///workspace/file.txt"
            }
        }"#,
    )
    .expect("valid resource ContentBlock should deserialize");
    match block {
        ContentBlock::Resource(r) => match &r.resource {
            ff_acp_types::content::EmbeddedResourceResource::Text(t) => {
                assert_eq!(t.text, "file content");
                assert_eq!(t.uri, "file:///workspace/file.txt");
            }
            other => panic!("expected Text resource, got {other:?}"),
        },
        other => panic!("expected Resource, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Negative tests: missing required fields → Err
// ---------------------------------------------------------------------------

#[test]
fn write_text_file_request_missing_session_id_fails() {
    let err = serde_json::from_str::<WriteTextFileRequest>(
        r#"{"path":"/workspace/main.rs","content":"fn main() {}"}"#,
    );
    assert!(err.is_err(), "missing sessionId should fail");
}

#[test]
fn write_text_file_request_missing_path_fails() {
    let err = serde_json::from_str::<WriteTextFileRequest>(
        r#"{"sessionId":"sess_01","content":"fn main() {}"}"#,
    );
    assert!(err.is_err(), "missing path should fail");
}

#[test]
fn write_text_file_request_missing_content_fails() {
    let err = serde_json::from_str::<WriteTextFileRequest>(
        r#"{"sessionId":"sess_01","path":"/workspace/main.rs"}"#,
    );
    assert!(err.is_err(), "missing content should fail");
}

#[test]
fn read_text_file_request_missing_path_fails() {
    let err = serde_json::from_str::<ReadTextFileRequest>(r#"{"sessionId":"sess_01"}"#);
    assert!(err.is_err(), "missing path should fail");
}

#[test]
fn read_text_file_response_missing_content_fails() {
    let err = serde_json::from_str::<ReadTextFileResponse>(r#"{}"#);
    assert!(err.is_err(), "missing content should fail");
}

#[test]
fn request_permission_request_missing_tool_call_fails() {
    let err =
        serde_json::from_str::<RequestPermissionRequest>(r#"{"sessionId":"sess_01","options":[]}"#);
    assert!(err.is_err(), "missing toolCall should fail");
}

#[test]
fn request_permission_request_missing_options_fails() {
    let err = serde_json::from_str::<RequestPermissionRequest>(
        r#"{"sessionId":"sess_01","toolCall":{"toolCallId":"tc_1","title":"test"}}"#,
    );
    assert!(err.is_err(), "missing options should fail");
}

#[test]
fn content_block_text_missing_text_fails() {
    let err = serde_json::from_str::<ContentBlock>(r#"{"type":"text"}"#);
    assert!(err.is_err(), "missing text field should fail");
}

#[test]
fn content_block_image_missing_data_fails() {
    let err = serde_json::from_str::<ContentBlock>(r#"{"type":"image","mimeType":"image/png"}"#);
    assert!(err.is_err(), "missing data should fail");
}

#[test]
fn content_block_image_missing_mime_type_fails() {
    let err = serde_json::from_str::<ContentBlock>(r#"{"type":"image","data":"iVBORw0KGgo="}"#);
    assert!(err.is_err(), "missing mimeType should fail");
}

#[test]
fn content_block_resource_missing_uri_fails() {
    let err = serde_json::from_str::<ContentBlock>(
        r#"{"type":"resource","resource":{"text":"content"}}"#,
    );
    assert!(err.is_err(), "missing uri on embedded resource should fail");
}

#[test]
fn initialize_request_missing_protocol_version_fails() {
    let err = serde_json::from_str::<InitializeRequest>(r#"{}"#);
    assert!(err.is_err(), "missing protocolVersion should fail");
}

#[test]
fn prompt_request_missing_prompt_fails() {
    let err = serde_json::from_str::<PromptRequest>(r#"{"sessionId":"sess_01"}"#);
    assert!(err.is_err(), "missing prompt should fail");
}

#[test]
fn new_session_request_missing_cwd_fails() {
    let err = serde_json::from_str::<NewSessionRequest>(r#"{"mcpServers":[]}"#);
    assert!(err.is_err(), "missing cwd should fail");
}

/// Verify that unknown fields are tolerated — the negative counterpart to
/// `deny_unknown_fields` on these types.
#[test]
fn write_text_file_request_with_extra_field_succeeds() {
    let payload = serde_json::from_str::<WriteTextFileRequest>(
        r#"{"sessionId":"sess_01","path":"/x","content":"x","unknownField":"ignored"}"#,
    );
    assert!(payload.is_ok(), "extra fields should be tolerated");
}

#[test]
fn content_block_text_with_extra_field_succeeds() {
    let block = serde_json::from_str::<ContentBlock>(
        r#"{"type":"text","text":"hello","extraField":"ignored"}"#,
    );
    assert!(
        block.is_ok(),
        "extra fields on content blocks should be tolerated"
    );
}
