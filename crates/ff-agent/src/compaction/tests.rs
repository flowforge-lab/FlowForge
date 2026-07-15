use super::*;
use async_trait::async_trait;
use ff_llm::{Chunk, ChunkStream, LlmError, ToolCallDelta};
use ff_memory::{FlushLedger, Fts5Index, Memory, MemoryConfig, MemoryIndex};
use ff_tools::memory::MemoryWriteTool;
use ff_tools::{Safety, Tool};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn msg(role: Role, content: &str) -> Message {
    Message {
        id: String::new(),
        session_id: String::new(),
        role,
        content: content.to_string(),
        tool_calls: None,
        tool_call_id: None,
        attachments: None,
        reasoning: None,
        stop_reason: None,
        author_name: None,
        created_at: 0,
    }
}

#[test]
fn proxy_estimator_counts_tokens() {
    let est = ProxyTokenEstimator { budget_tokens: 100 };
    // "x" repeated 40 times: tokenx-rs estimates ~7 tokens (a single long
    // alphanumeric word scored by ceil(len / chars_per_token)).
    let history = vec![msg(Role::User, &"x".repeat(40))];
    let p = est.assess(&history, "any-model");
    assert!(
        p.estimated_tokens > 0,
        "non-empty content must produce tokens"
    );
    assert_eq!(p.budget_tokens, 100);
    let frac = p.estimated_tokens as f64 / 100.0;
    assert!(frac > 0.0 && frac < 0.75, "well under the flush threshold");
    assert!(!p.is_over(0.75));
}

#[test]
fn proxy_estimator_counts_reasoning() {
    // Persisted reasoning is replayed on the wire (#378), so it must count
    // toward context pressure alongside content.
    let est = ProxyTokenEstimator { budget_tokens: 100 };
    let content_only = msg(Role::Assistant, &"x".repeat(40));
    let p_content = est.assess(&[content_only], "any-model");

    let mut with_reasoning = msg(Role::Assistant, &"x".repeat(40));
    with_reasoning.reasoning = Some("y".repeat(40));
    let p_both = est.assess(&[with_reasoning], "any-model");

    assert!(
        p_both.estimated_tokens > p_content.estimated_tokens,
        "reasoning must add to the estimate: content={} both={}",
        p_content.estimated_tokens,
        p_both.estimated_tokens
    );
}

#[test]
fn pressure_is_over_at_threshold() {
    let p = ContextPressure {
        estimated_tokens: 80,
        budget_tokens: 100,
    };
    assert!(p.is_over(0.75));
    assert!(!p.is_over(0.9));
}

#[test]
fn zero_budget_never_trips() {
    let p = ContextPressure {
        estimated_tokens: 999,
        budget_tokens: 0,
    };
    assert_eq!(p.fraction(), 0.0);
    assert!(!p.is_over(0.01));
}

/// Records the tool calls it received, so a test can assert what the flush ran.
struct RecordingTool {
    name: &'static str,
    seen: Arc<std::sync::Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for RecordingTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "test tool"
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object", "properties": {} })
    }
    fn safety(&self, _args: &serde_json::Value) -> Safety {
        Safety::Write
    }
    async fn run(&self, args: serde_json::Value, _root: &Path) -> ToolOutcome {
        self.seen.lock().unwrap().push(args.to_string());
        ToolOutcome::ok("written")
    }
}

fn registry_with(seen: Arc<std::sync::Mutex<Vec<String>>>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(RecordingTool {
        name: "memory_write",
        seen: seen.clone(),
    }));
    // A non-memory tool that should never be offered to or run by a flush.
    reg.register(Box::new(RecordingTool { name: "bash", seen }));
    reg
}

/// One call requesting `memory_write`, then plain text.
struct WriteThenText {
    calls: AtomicUsize,
}
#[async_trait]
impl Provider for WriteThenText {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        // The flush must only advertise memory tools.
        let names: Vec<&str> = req
            .tools
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"memory_write"));
        assert!(!names.contains(&"bash"));

        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let chunks = if n == 0 {
            vec![Ok(Chunk {
                tool_calls: vec![ToolCallDelta {
                    index: 0,
                    id: Some("w1".into()),
                    name: Some("memory_write".into()),
                    arguments: r#"{"text":"user prefers dark mode"}"#.into(),
                }],
                done: true,
                ..Chunk::default()
            })]
        } else {
            vec![Ok(Chunk {
                delta: "saved".into(),
                done: true,
                ..Chunk::default()
            })]
        };
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

/// Replies `NO_REPLY` with no tool calls.
struct NoReplyProvider;
#[async_trait]
impl Provider for NoReplyProvider {
    async fn chat_stream(&self, _req: ChatRequest) -> Result<ChunkStream, LlmError> {
        Ok(futures_util::stream::iter(vec![Ok(Chunk {
            delta: "NO_REPLY".into(),
            done: true,
            ..Chunk::default()
        })])
        .boxed())
    }
}

fn store_with_history() -> (SessionStore, String) {
    let store = SessionStore::new();
    let s = store.create_session(None);
    store.add_message(&s.id, Role::User, "I prefer dark mode everywhere.".into());
    store.add_message(&s.id, Role::Assistant, "Noted.".into());
    (store, s.id)
}

#[tokio::test]
async fn flush_writes_durable_fact_via_memory_write_only() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let registry = registry_with(seen.clone());
    let (store, sid) = store_with_history();
    let provider = WriteThenText {
        calls: AtomicUsize::new(0),
    };
    let dir = tempfile::tempdir().unwrap();

    let outcome = MemoryFlush
        .compact(CompactionContext {
            provider: &provider,
            store: &store,
            registry: &registry,
            root: dir.path(),
            session_id: &sid,
            model: "mock",
            cancel: CancelToken::new(),
        })
        .await
        .unwrap();

    assert_eq!(outcome, CompactionOutcome::Wrote { writes: 1 });
    let calls = seen.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].contains("dark mode"));

    // The flush is transcript-silent: the visible history is untouched.
    assert_eq!(store.get_messages(&sid).len(), 2);
}

#[tokio::test]
async fn flush_no_reply_writes_nothing() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let registry = registry_with(seen.clone());
    let (store, sid) = store_with_history();
    let provider = NoReplyProvider;
    let dir = tempfile::tempdir().unwrap();

    let outcome = MemoryFlush
        .compact(CompactionContext {
            provider: &provider,
            store: &store,
            registry: &registry,
            root: dir.path(),
            session_id: &sid,
            model: "mock",
            cancel: CancelToken::new(),
        })
        .await
        .unwrap();

    assert_eq!(outcome, CompactionOutcome::NoReply);
    assert!(seen.lock().unwrap().is_empty());
    assert_eq!(store.get_messages(&sid).len(), 2);
}

#[tokio::test]
async fn flush_on_empty_session_is_noreply() {
    let registry = registry_with(Arc::new(std::sync::Mutex::new(Vec::new())));
    let store = SessionStore::new();
    let s = store.create_session(None);
    let provider = NoReplyProvider;
    let dir = tempfile::tempdir().unwrap();

    let outcome = MemoryFlush
        .compact(CompactionContext {
            provider: &provider,
            store: &store,
            registry: &registry,
            root: dir.path(),
            session_id: &s.id,
            model: "mock",
            cancel: CancelToken::new(),
        })
        .await
        .unwrap();
    assert_eq!(outcome, CompactionOutcome::NoReply);
}
#[test]
fn flush_due_respects_threshold_and_interval() {
    let under = ContextPressure {
        estimated_tokens: 50,
        budget_tokens: 100,
    };
    let over = ContextPressure {
        estimated_tokens: 80,
        budget_tokens: 100,
    };
    // Below threshold: never due.
    assert!(!flush_due(under, 100, None, 0.75, 40));
    // Over threshold, never flushed: due.
    assert!(flush_due(over, 30, None, 0.75, 40));
    // Over threshold, flushed recently (grew only 10 since): not due.
    assert!(!flush_due(over, 40, Some(30), 0.75, 40));
    // Over threshold, grew a full interval since the last flush: due again.
    assert!(flush_due(over, 70, Some(30), 0.75, 40));
}

// -----------------------------------------------------------------------
// End-to-end flush path (M5.2 pre-compaction memory-flush, #165).
//
// The component tests above prove the flush's tool-loop mechanics with a mock
// RecordingTool and a mock Provider. These tests drive the *real* path
// end-to-end: a real `MemoryWriteTool` against a temp-dir `Memory` and a real
// FTS5 index, so a durable fact is asserted on disk in the real
// `daily/YYYY-MM-DD.md` file — not just at the tool-call seam. They also cover
// the NO_REPLY path (no provider write -> no file mutation) and the
// once-per-cycle ledger gate (a second call in the same cycle does not
// re-flush), locking the compaction boundary against regressions.
// -----------------------------------------------------------------------

/// A real on-disk `Memory` (temp dir) backed by an in-memory FTS5 index — the
/// real pair production wires through `MemoryWriteTool`.
fn real_memory(dir: &Path) -> (Arc<Memory>, Arc<dyn MemoryIndex>) {
    let memory = Arc::new(Memory::new(dir.to_path_buf(), MemoryConfig::default()));
    let index: Arc<dyn MemoryIndex> = Arc::new(Fts5Index::open_in_memory().unwrap());
    (memory, index)
}

/// Register only the real `MemoryWriteTool`, so the flush advertises exactly
// the memory tools (matching production, where the flush filters to
// `memory_*`).
fn real_memory_registry(memory: Arc<Memory>, index: Arc<dyn MemoryIndex>) -> ToolRegistry {
    let mut reg = ToolRegistry::new();
    reg.register(Box::new(MemoryWriteTool::new(memory, index)));
    reg
}

#[tokio::test]
async fn e2e_flush_writes_durable_fact_to_real_daily_log() {
    let dir = tempfile::tempdir().unwrap();
    let (memory, index) = real_memory(dir.path());
    let registry = real_memory_registry(memory.clone(), index);
    let (store, sid) = store_with_history();
    let provider = WriteThenText {
        calls: AtomicUsize::new(0),
    };

    let outcome = MemoryFlush
        .compact(CompactionContext {
            provider: &provider,
            store: &store,
            registry: &registry,
            root: dir.path(),
            session_id: &sid,
            model: "mock",
            cancel: CancelToken::new(),
        })
        .await
        .unwrap();

    assert_eq!(outcome, CompactionOutcome::Wrote { writes: 1 });

    // The durable fact landed in the real on-disk daily log
    // (`<root>/daily/YYYY-MM-DD.md`), written by the real MemoryWriteTool.
    let today = chrono::Local::now().date_naive();
    let daily = memory.daily_path(today);
    let on_disk = std::fs::read_to_string(&daily).unwrap_or_else(|_| {
        panic!(
            "expected daily log at {} after a writing flush",
            daily.display()
        )
    });
    assert!(
        on_disk.contains("user prefers dark mode"),
        "durable fact missing from {}: {on_disk}",
        daily.display()
    );

    // The flush is transcript-silent: the visible history is untouched.
    assert_eq!(store.get_messages(&sid).len(), 2);
}

#[tokio::test]
async fn e2e_flush_no_reply_leaves_no_file_on_disk() {
    let dir = tempfile::tempdir().unwrap();
    let (memory, index) = real_memory(dir.path());
    let registry = real_memory_registry(memory.clone(), index);
    let (store, sid) = store_with_history();
    let provider = NoReplyProvider;

    // Snapshot the temp dir's file tree before the flush so the assertion is
    // independent of any pre-existing scaffolding.
    let before: std::collections::HashSet<_> = std::fs::read_dir(dir.path())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();

    let outcome = MemoryFlush
        .compact(CompactionContext {
            provider: &provider,
            store: &store,
            registry: &registry,
            root: dir.path(),
            session_id: &sid,
            model: "mock",
            cancel: CancelToken::new(),
        })
        .await
        .unwrap();

    assert_eq!(outcome, CompactionOutcome::NoReply);

    // No provider write -> no daily log file and no `daily/` directory
    // created (Memory::write creates the parent dir only when it writes).
    let today = chrono::Local::now().date_naive();
    assert!(
        !memory.daily_path(today).exists(),
        "NO_REPLY flush must not create a daily log file"
    );
    assert!(
        !memory.root().join("daily").exists(),
        "NO_REPLY flush must not create the daily/ directory at all"
    );
    assert!(
        !memory.curated_path().exists(),
        "NO_REPLY flush must not touch curated memory"
    );

    // The temp dir's file tree is unchanged: zero file mutation.
    let after: std::collections::HashSet<_> = std::fs::read_dir(dir.path())
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(
        before, after,
        "NO_REPLY flush must not mutate the filesystem"
    );

    assert_eq!(store.get_messages(&sid).len(), 2);
}

#[tokio::test]
async fn e2e_ledger_gate_flushes_once_per_cycle() {
    // Faithful replication of the host seam (`FlowForgeState::maybe_flush_memory`):
    // a real `ProxyTokenEstimator` + real on-disk `FlushLedger` gate a real
    // `MemoryFlush.compact()` driving the real `MemoryWriteTool`. The first
    // over-budget call flushes and records the cycle marker; a second call in
    // the same cycle (transcript unchanged) is blocked by `flush_due` and does
    // not re-flush.
    let dir = tempfile::tempdir().unwrap();
    let (memory, index) = real_memory(dir.path());
    let registry = real_memory_registry(memory.clone(), index);
    let (store, sid) = store_with_history();
    let provider = WriteThenText {
        calls: AtomicUsize::new(0),
    };

    // A small budget so the short 2-message history is genuinely over budget,
    // exercising the same `ProxyTokenEstimator` the host seam uses (the host
    // owns the real value; the estimator type is the production type).
    let estimator = ProxyTokenEstimator { budget_tokens: 4 };
    let ledger = FlushLedger::open(dir.path().join("flush.db")).unwrap();
    let cancel = CancelToken::new();
    let model = "mock";
    // Mirrors `REFLUSH_INTERVAL_MESSAGES` in the desktop host seam.
    const REFLUSH_INTERVAL: u64 = 40;

    // --- First call: over budget, never flushed -> the gate fires. ---
    let history = store.get_messages(&sid);
    let pressure = estimator.assess(&history, model);
    let message_count = history.len() as u64;
    let last = ledger.last_flush(&sid).unwrap().map(|r| r.message_count);
    assert!(
        flush_due(
            pressure,
            message_count,
            last,
            DEFAULT_FLUSH_AT_FRACTION,
            REFLUSH_INTERVAL
        ),
        "first over-budget call must be due"
    );

    let outcome = MemoryFlush
        .compact(CompactionContext {
            provider: &provider,
            store: &store,
            registry: &registry,
            root: dir.path(),
            session_id: &sid,
            model,
            cancel: cancel.clone(),
        })
        .await
        .unwrap();
    assert_eq!(outcome, CompactionOutcome::Wrote { writes: 1 });
    // The host seam records the cycle marker after a successful flush.
    ledger
        .record_flush(&sid, message_count, chrono::Utc::now().timestamp_millis())
        .unwrap();
    let calls_after_first = provider.calls.load(Ordering::SeqCst);
    assert!(
        calls_after_first > 0,
        "expected the provider to be driven by the first flush"
    );

    // --- Second call: same cycle (transcript unchanged) -> gate blocks. ---
    let history = store.get_messages(&sid);
    let pressure = estimator.assess(&history, model);
    let message_count = history.len() as u64;
    let last = ledger.last_flush(&sid).unwrap().map(|r| r.message_count);
    assert!(
        !flush_due(
            pressure,
            message_count,
            last,
            DEFAULT_FLUSH_AT_FRACTION,
            REFLUSH_INTERVAL
        ),
        "a second call in the same cycle must not be due"
    );
    // The host seam returns here without invoking `compact()`. Assert the
    // provider saw no additional calls (i.e. `compact()` was not re-run).
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        calls_after_first,
        "compact() must not be re-invoked within the same cycle"
    );

    // The ledger records exactly one flush for this session, at this cycle
    // marker — the durable bookkeeping that survives to block the next call.
    let rec = ledger.last_flush(&sid).unwrap().unwrap();
    assert_eq!(rec.message_count, message_count);

    // And exactly one durable fact landed on disk (no duplicate write).
    let today = chrono::Local::now().date_naive();
    let on_disk = std::fs::read_to_string(memory.daily_path(today)).unwrap_or_default();
    assert_eq!(
        on_disk.matches("user prefers dark mode").count(),
        1,
        "expected exactly one durable fact on disk, got: {on_disk}"
    );

    // Growing past the reflush interval re-arms the gate (the cycle advances).
    let grown = message_count + REFLUSH_INTERVAL;
    assert!(
        flush_due(
            pressure,
            grown,
            Some(message_count),
            DEFAULT_FLUSH_AT_FRACTION,
            REFLUSH_INTERVAL
        ),
        "a full interval of growth must re-arm the gate for the next cycle"
    );
}
