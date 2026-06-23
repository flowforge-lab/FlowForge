//! The `compaction_retrieve` tool (M7.1a, RFC 0016 Tier 1).
//!
//! When a tool result is large, the agent loop compacts it before it enters the
//! transcript and stashes the verbatim original in the session store, keyed by
//! the content hash carried in the trailing `[compacted; retrieve key=...]`
//! marker. This tool lets the model pull that original back on demand, so the
//! compaction is *reversible*: detail is never lost, only deferred. Read-only --
//! it never mutates state, so the agent loop auto-runs it without approval.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use ff_session::SessionStore;
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome};

/// Retrieve the verbatim original of a previously-compacted tool result.
pub struct CompactionRetrieveTool {
    store: Arc<SessionStore>,
}

impl CompactionRetrieveTool {
    #[must_use]
    pub fn new(store: Arc<SessionStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for CompactionRetrieveTool {
    fn name(&self) -> &str {
        "compaction_retrieve"
    }

    fn description(&self) -> &str {
        "Retrieve the verbatim original of a previously-compacted tool result. \
         When a tool result ends with a `[compacted; retrieve key=<HEX>]` marker, \
         its content was abbreviated to save context; call this with that key to \
         read the unabridged original when you need detail the summary dropped."
    }

    fn parameters(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "key": {
                    "type": "string",
                    "description": "The hex key from the `[compacted; retrieve key=<HEX>]` marker."
                }
            },
            "required": ["key"]
        })
    }

    fn safety(&self, _args: &Value) -> Safety {
        Safety::ReadOnly
    }

    fn max_safety(&self) -> Safety {
        Safety::ReadOnly
    }

    async fn run(&self, args: Value, _root: &Path) -> ToolOutcome {
        let Some(key) = args.get("key").and_then(Value::as_str) else {
            return ToolOutcome::error("missing required argument: key (a string)");
        };
        match self.store.compaction_original(key) {
            Some(content) => ToolOutcome::ok(content),
            None => ToolOutcome::error(format!(
                "no compacted original found for key `{key}` (it may never have been \
                 compacted, or its session was deleted)"
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn retrieves_a_stored_original() {
        let store = Arc::new(SessionStore::new());
        let s = store.create_session(None);
        let m = store.add_tool_result_message(&s.id, "call-1".into(), "compressed".into());
        store.put_compaction_original(&s.id, &m.id, "deadbeefdeadbeef", "the full original");

        let tool = CompactionRetrieveTool::new(store);
        let out = tool
            .run(
                serde_json::json!({ "key": "deadbeefdeadbeef" }),
                Path::new("."),
            )
            .await;
        assert!(out.success);
        assert_eq!(out.content, "the full original");
    }

    #[tokio::test]
    async fn missing_key_is_a_clean_error() {
        let store = Arc::new(SessionStore::new());
        let tool = CompactionRetrieveTool::new(store);
        let out = tool
            .run(serde_json::json!({ "key": "nope" }), Path::new("."))
            .await;
        assert!(!out.success);
        assert!(out.content.contains("no compacted original"));
    }

    #[tokio::test]
    async fn missing_argument_is_a_clean_error() {
        let store = Arc::new(SessionStore::new());
        let tool = CompactionRetrieveTool::new(store);
        let out = tool.run(serde_json::json!({}), Path::new(".")).await;
        assert!(!out.success);
        assert!(out.content.contains("missing required argument"));
    }
}
