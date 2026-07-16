use super::*;
use async_trait::async_trait;
use ff_llm::{Chunk, ChunkStream, LlmError};
use futures_util::StreamExt;

/// Emits a fixed summary as one streamed text answer. Records the request so
/// tests can assert which model the summarizer asked for.
struct CannedSummary {
    text: String,
    model_seen: std::sync::Mutex<Option<String>>,
}

impl CannedSummary {
    fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            model_seen: std::sync::Mutex::new(None),
        }
    }
}

#[async_trait]
impl Provider for CannedSummary {
    async fn chat_stream(&self, req: ChatRequest) -> Result<ChunkStream, LlmError> {
        *self.model_seen.lock().unwrap() = Some(req.model.clone());
        let chunks = vec![Ok(Chunk {
            delta: self.text.clone(),
            done: true,
            ..Chunk::default()
        })];
        Ok(futures_util::stream::iter(chunks).boxed())
    }
}

fn msg(id: &str, role: Role, content: &str) -> Message {
    Message {
        id: id.to_string(),
        session_id: "s1".to_string(),
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

/// A cold transcript long enough to be worth summarizing: many wordy turns.
fn long_transcript(cold: usize, recent: usize) -> Vec<Message> {
    let mut v = Vec::new();
    for i in 0..cold {
        let body = format!(
            "This is cold message {i} carrying a fair amount of detail about prior work, \
             decisions, identifiers, and tool output that we want condensed."
        );
        let role = if i % 2 == 0 {
            Role::User
        } else {
            Role::Assistant
        };
        v.push(msg(&format!("c{i}"), role, &body));
    }
    for i in 0..recent {
        v.push(msg(&format!("r{i}"), Role::User, "recent exact state"));
    }
    v
}

fn enabled() -> AbstractiveConfig {
    AbstractiveConfig {
        enabled: true,
        ..AbstractiveConfig::default()
    }
}

#[tokio::test]
async fn disabled_is_a_no_op() {
    let s = AbstractiveSummarizer::default();
    let msgs = long_transcript(10, 2);
    let out = s
        .summarize_cold(&CannedSummary::new("x"), "m", &msgs, 2, &CancelToken::new())
        .await
        .unwrap();
    assert!(out.is_none(), "default config is disabled");
}

#[tokio::test]
async fn short_cold_prefix_is_skipped() {
    let s = AbstractiveSummarizer::new(enabled());
    // 3 cold + 2 recent: cold_end = 3 < min_cold_messages (4) -> skip.
    let msgs = long_transcript(3, 2);
    let out = s
        .summarize_cold(&CannedSummary::new("x"), "m", &msgs, 2, &CancelToken::new())
        .await
        .unwrap();
    assert!(out.is_none());
}

#[tokio::test]
async fn summarizes_cold_keeps_recent_and_is_retrievable() {
    let s = AbstractiveSummarizer::new(enabled());
    let msgs = long_transcript(10, 2);
    let provider = CannedSummary::new("Condensed summary of the earlier work.");
    let result = s
        .summarize_cold(&provider, "session-model", &msgs, 2, &CancelToken::new())
        .await
        .unwrap()
        .expect("a long cold prefix must summarize");

    // Wire = [summary] + the 2 recent verbatim messages.
    assert_eq!(result.messages.len(), 3);
    assert_eq!(result.messages[0].role, Role::User);
    assert!(result.messages[0].content.contains("Condensed summary"));
    assert_eq!(result.messages[1].content, "recent exact state");
    assert_eq!(result.messages[2].content, "recent exact state");
    assert_eq!(result.boundary, 10);

    // Reversible: the marker key matches the persisted original (the cold block).
    let (_, key, original) = result.original.expect("collapsed block must be stored");
    assert!(result.messages[0]
        .content
        .contains(&format!("{COMPACTION_MARKER_PREFIX}{key}]")));
    assert!(original.contains("cold message 0"));
    assert!(original.contains("cold message 9"));

    // It actually shrank.
    assert!(result.savings.saved() > 0);
}

#[tokio::test]
async fn empty_model_override_uses_session_model() {
    let s = AbstractiveSummarizer::new(enabled());
    let provider = CannedSummary::new("summary text that is reasonably long to win");
    let msgs = long_transcript(10, 2);
    s.summarize_cold(
        &provider,
        "the-session-model",
        &msgs,
        2,
        &CancelToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        provider.model_seen.lock().unwrap().as_deref(),
        Some("the-session-model")
    );
}

#[tokio::test]
async fn model_override_is_used_when_set() {
    let s = AbstractiveSummarizer::new(AbstractiveConfig {
        enabled: true,
        model: Some("cheap-summarizer".to_string()),
        ..AbstractiveConfig::default()
    });
    let provider = CannedSummary::new("summary text that is reasonably long to win");
    let msgs = long_transcript(10, 2);
    s.summarize_cold(
        &provider,
        "the-session-model",
        &msgs,
        2,
        &CancelToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        provider.model_seen.lock().unwrap().as_deref(),
        Some("cheap-summarizer")
    );
}

#[tokio::test]
async fn non_shrinking_summary_is_rejected() {
    let s = AbstractiveSummarizer::new(enabled());
    // The "summary" is far larger than the cold block -> must not substitute.
    let huge = "x".repeat(10_000);
    let provider = CannedSummary::new(&huge);
    let msgs = long_transcript(6, 2);
    let out = s
        .summarize_cold(&provider, "m", &msgs, 2, &CancelToken::new())
        .await
        .unwrap();
    assert!(out.is_none(), "a summary that does not shrink is dropped");
}

#[tokio::test]
async fn empty_summary_is_rejected() {
    let s = AbstractiveSummarizer::new(enabled());
    let provider = CannedSummary::new("   ");
    let msgs = long_transcript(10, 2);
    let out = s
        .summarize_cold(&provider, "m", &msgs, 2, &CancelToken::new())
        .await
        .unwrap();
    assert!(out.is_none());
}

#[test]
fn summary_due_fires_first_then_on_growth() {
    // First sight: due.
    assert!(summary_due(20, None, 8));
    // Within the window: not due (reuse the cached summary).
    assert!(!summary_due(24, Some(20), 8));
    // Grown a full interval: due again.
    assert!(summary_due(28, Some(20), 8));
    assert!(summary_due(30, Some(20), 8));
}

#[test]
fn render_cold_skips_empty_and_labels_roles() {
    let msgs = vec![
        msg("a", Role::User, "hello"),
        msg("b", Role::Assistant, ""),
        msg("c", Role::Tool, "tool out"),
    ];
    let rendered = render_cold(&msgs);
    assert!(rendered.contains("User: hello"));
    assert!(rendered.contains("Tool: tool out"));
    assert!(!rendered.contains("Assistant:"), "empty content is skipped");
}

// ----- #972: cap the summarizer input -----

fn enabled_with_cap(cap: usize) -> AbstractiveConfig {
    AbstractiveConfig {
        enabled: true,
        max_summary_input_tokens: cap,
        ..AbstractiveConfig::default()
    }
}

#[test]
fn capped_cold_end_unbounded_returns_full_cold() {
    let msgs = long_transcript(10, 2);
    // cap 0 = legacy: the whole cold prefix (cold_end = 10).
    assert_eq!(capped_cold_end(&msgs, 10, 0), 10);
}

#[test]
fn capped_cold_end_bounds_to_oldest_slice() {
    let msgs = long_transcript(10, 2);
    // Each cold message is ~30 proxy tokens; a 60-token cap admits ~2 of them.
    let end = capped_cold_end(&msgs, 10, 60);
    assert!((1..10).contains(&end), "cap must bite: got {end}");
    // The admitted slice must not exceed the cap (allowing the first, always-kept one).
    let admitted: usize = msgs[..end].iter().map(|m| proxy_tokens(&m.content)).sum();
    let one_more: usize = proxy_tokens(&msgs[end].content);
    assert!(
        admitted <= 60 || end == 1,
        "admitted {admitted} exceeds cap without being the single-message floor"
    );
    assert!(
        admitted + one_more > 60,
        "should have stopped earlier if room remained"
    );
}

#[test]
fn capped_cold_end_always_makes_progress_on_oversized_message() {
    // A single giant oldest message far exceeding the cap must still be admitted
    // (end >= 1), or the summarizer would stall forever.
    let big = "x".repeat(400_000); // ~100K proxy tokens
    let msgs = vec![
        msg("c0", Role::Tool, &big),
        msg("c1", Role::User, "small"),
        msg("r0", Role::User, "recent"),
    ];
    assert_eq!(
        capped_cold_end(&msgs, 2, 24_000),
        1,
        "oversized oldest msg still admitted"
    );
}

#[tokio::test]
async fn cap_summarizes_only_oldest_slice_and_keeps_remainder_verbatim() {
    let s = AbstractiveSummarizer::new(enabled_with_cap(60));
    let msgs = long_transcript(10, 2); // 10 cold + 2 recent
    let out = s
        .summarize_cold(
            &CannedSummary::new("CONDENSED"),
            "m",
            &msgs,
            2,
            &CancelToken::new(),
        )
        .await
        .unwrap()
        .expect("summary produced");

    // Boundary is capped well below cold_end (10).
    assert!(
        (1..10).contains(&out.boundary),
        "boundary capped: {}",
        out.boundary
    );
    // Output = [summary] + everything from boundary onward, verbatim.
    assert_eq!(out.messages.len(), 1 + (msgs.len() - out.boundary));
    assert!(out.messages[0].content.contains("CONDENSED"));
    assert!(out.messages[0].content.contains(COMPACTION_MARKER_PREFIX));
    for (k, m) in msgs[out.boundary..].iter().enumerate() {
        assert_eq!(
            out.messages[1 + k].content,
            m.content,
            "remainder stays verbatim"
        );
    }
    // Reversibility: the persisted original is exactly the capped slice rendered.
    let (mid, _key, original) = out.original.expect("original persisted");
    assert_eq!(
        mid,
        msgs[out.boundary - 1].id,
        "keyed on last summarized msg id"
    );
    assert_eq!(original, render_cold(&msgs[..out.boundary]));
    // Label reflects the actual (capped) count.
    assert!(out.messages[0]
        .content
        .contains(&format!("Summary of {} earlier", out.boundary)));
}

#[tokio::test]
async fn uncapped_summarizes_whole_cold_prefix() {
    // cap 0 reproduces legacy behavior: boundary == cold_end.
    let s = AbstractiveSummarizer::new(enabled_with_cap(0));
    let msgs = long_transcript(10, 2);
    let out = s
        .summarize_cold(&CannedSummary::new("C"), "m", &msgs, 2, &CancelToken::new())
        .await
        .unwrap()
        .expect("summary produced");
    assert_eq!(out.boundary, 10, "uncapped covers the whole cold prefix");
}
