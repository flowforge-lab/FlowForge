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
use ff_memory::{chunk_key, MemoryIndex};
use ff_session::SessionStore;
use serde_json::Value;

use crate::registry::{Safety, Tool, ToolOutcome};

/// The tool name, single-sourced so the agent loop can identify a retrieve
/// call without a magic string (mirrors `AGENT_TOOL_NAME`).
pub const COMPACTION_RETRIEVE_TOOL: &str = "compaction_retrieve";

/// How many top chunks to map from the retrieved content text (B2).
const TOP_K_MAPPING: usize = 10;

/// Retrieve the verbatim original of a previously-compacted tool result.
pub struct CompactionRetrieveTool {
    store: Arc<SessionStore>,
    /// On-disk memory index. A successful retrieve is a strong use-time
    /// reinforcement signal (RFC 0022 §4.3) that would otherwise evaporate with
    /// the session; we persist a cross-session count here and map the retrieved
    /// content to related chunks. `None` in contexts wired without a memory
    /// store, in which case recording and mapping are skipped.
    index: Option<Arc<dyn MemoryIndex>>,
}

impl CompactionRetrieveTool {
    #[must_use]
    pub fn new(store: Arc<SessionStore>) -> Self {
        Self { store, index: None }
    }

    /// Attach the memory index so a successful retrieve records a durable,
    /// cross-session reinforcement signal (RFC 0022 §4.3) and maps the
    /// retrieved content to related chunks. Builder so existing call sites
    /// keep compiling; omit it and recording is a no-op.
    #[must_use]
    pub fn with_index(mut self, index: Arc<dyn MemoryIndex>) -> Self {
        self.index = Some(index);
        self
    }
}

#[async_trait]
impl Tool for CompactionRetrieveTool {
    fn reaches_network(&self) -> bool {
        false
    }
    fn name(&self) -> &str {
        COMPACTION_RETRIEVE_TOOL
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
            Some(content) => {
                // A successful retrieve is a strong use-time reinforcement signal
                // (RFC 0022 §4.3). Persist a cross-session count in the derived
                // on-disk index. Best-effort and off the in-context path: it never
                // mutates the transcript/prompt cache, so `safety` stays ReadOnly.
                if let Some(index) = &self.index {
                    let _ = index.record_retrieve(key);
                    // Map the retrieved content to related chunks via FTS5
                    // text overlap (B2). The mapping is used by memory_search
                    // to boost the ranking of related chunks.
                    if let Ok(hits) = index.search(&content, TOP_K_MAPPING) {
                        let mappings: Vec<(String, f32)> = hits
                            .iter()
                            .map(|s| (chunk_key(&s.chunk), s.score))
                            .collect();
                        if !mappings.is_empty() {
                            let _ = index.map_retrieve_to_chunks(key, &mappings);
                        }
                    }
                }
                ToolOutcome::ok(content)
            }
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

    #[tokio::test]
    async fn successful_retrieve_records_a_durable_signal() {
        let store = Arc::new(SessionStore::new());
        let s = store.create_session(None);
        let m = store.add_tool_result_message(&s.id, "call-1".into(), "compressed".into());
        store.put_compaction_original(&s.id, &m.id, "deadbeef", "the full original");

        let index: Arc<dyn ff_memory::MemoryIndex> =
            Arc::new(ff_memory::Fts5Index::open_in_memory().unwrap());
        let tool = CompactionRetrieveTool::new(store).with_index(index.clone());

        // Two successful retrieves of the same key accumulate a cross-session count.
        for _ in 0..2 {
            let out = tool
                .run(serde_json::json!({ "key": "deadbeef" }), Path::new("."))
                .await;
            assert!(out.success);
        }
        assert_eq!(index.retrieve_count("deadbeef").unwrap(), 2);
    }

    #[tokio::test]
    async fn failed_retrieve_records_nothing() {
        let store = Arc::new(SessionStore::new());
        let index: Arc<dyn ff_memory::MemoryIndex> =
            Arc::new(ff_memory::Fts5Index::open_in_memory().unwrap());
        let tool = CompactionRetrieveTool::new(store).with_index(index.clone());

        let out = tool
            .run(serde_json::json!({ "key": "nope" }), Path::new("."))
            .await;
        assert!(!out.success);
        assert_eq!(
            index.retrieve_count("nope").unwrap(),
            0,
            "a miss is not a use signal"
        );
    }

    #[tokio::test]
    async fn retrieve_without_index_is_a_noop_not_a_panic() {
        let store = Arc::new(SessionStore::new());
        let s = store.create_session(None);
        let m = store.add_tool_result_message(&s.id, "call-1".into(), "compressed".into());
        store.put_compaction_original(&s.id, &m.id, "deadbeef", "the full original");

        let tool = CompactionRetrieveTool::new(store);
        let out = tool
            .run(serde_json::json!({ "key": "deadbeef" }), Path::new("."))
            .await;
        assert!(out.success);
        assert_eq!(out.content, "the full original");
    }

    #[tokio::test]
    async fn successful_retrieve_creates_content_chunk_mapping() {
        let store = Arc::new(SessionStore::new());
        let s = store.create_session(None);
        let m = store.add_tool_result_message(&s.id, "call-1".into(), "compressed".into());
        // The retrieved content mentions "rust" so it should match the indexed chunk.
        store.put_compaction_original(&s.id, &m.id, "key1", "rust is a systems language");

        let index: Arc<dyn ff_memory::MemoryIndex> =
            Arc::new(ff_memory::Fts5Index::open_in_memory().unwrap());
        // Seed a chunk about rust.
        let md = "## Rust\nrust is a systems language";
        let cs = ff_memory::chunk_markdown(
            md,
            ff_memory::MemorySource::Curated,
            std::path::Path::new("MEMORY.md"),
        );
        index.reindex(&cs).unwrap();

        let rust_key = ff_memory::chunk_key(&cs[0]);
        let tool = CompactionRetrieveTool::new(store).with_index(index.clone());

        let out = tool
            .run(serde_json::json!({ "key": "key1" }), Path::new("."))
            .await;
        assert!(out.success);

        // The retrieve should have created a mapping: key1 → rust chunk.
        let hits = index
            .chunk_retrieve_hits(std::slice::from_ref(&rust_key))
            .unwrap();
        assert_eq!(
            hits.get(&rust_key).copied(),
            Some(1),
            "a mapping must exist for the related chunk"
        );
    }

    #[tokio::test]
    async fn failed_retrieve_creates_no_mapping() {
        let store = Arc::new(SessionStore::new());
        let index: Arc<dyn ff_memory::MemoryIndex> =
            Arc::new(ff_memory::Fts5Index::open_in_memory().unwrap());
        let tool = CompactionRetrieveTool::new(store).with_index(index.clone());

        let out = tool
            .run(serde_json::json!({ "key": "nope" }), Path::new("."))
            .await;
        assert!(!out.success);

        // No mapping should exist for the failed key.
        let hits = index
            .chunk_retrieve_hits(&["chunk:nope".to_string()])
            .unwrap();
        assert!(hits.is_empty(), "no mapping for a failed retrieve");
    }
}
