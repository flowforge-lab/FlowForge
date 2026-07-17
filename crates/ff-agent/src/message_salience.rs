//! Value-aware salience for cold session messages (#933 / RFC 0022 Step 2b).
//!
//! Reuses the memory subsystem's [`Salience<T>`](ff_memory::Salience) *model* — the
//! same trait that ranks memory chunks for promotion/demotion (RFC 0007) — applied
//! to transcript [`Message`]s so graded compaction (#970) can keep *important* older
//! messages sharp while folding low-value bulk harder.
//!
//! ## Cache-stability constraint (the load-bearing design rule)
//!
//! Graded compaction chooses a message's band, and the A.2 frozen-boundary cache
//! (#933) relies on that choice being **identical every turn** for a given message —
//! otherwise the cached prefix bytes drift and the prompt cache is busted every turn
//! (the exact failure #968 avoided by keying bands on absolute index).
//!
//! Therefore [`MessageSalience`] scores **only signals that are a pure function of
//! the message's own content** (role, size), which never change once the message
//! exists. It deliberately does **not** reuse memory's recency (it slides with
//! wall-clock/`cold_end`) or frequency (needs a later-reference scan and also
//! slides) — those would make the same message score differently across turns and
//! defeat the cache. Frequency/recency are a documented Step-2c follow-up; the
//! `occurrences` argument of the trait is accepted for signature compatibility and
//! ignored here.

use ff_core::{Message, Role};
use ff_memory::Salience;

use crate::compaction_extractive::proxy_tokens;

/// Content-only value scorer for a cold transcript message. Higher = more worth
/// keeping sharp (a shallower/gentler compaction band); lower = fold harder.
///
/// All inputs are pure functions of the message, so the score is stable across
/// turns — the invariant graded compaction's cache depends on.
#[derive(Debug, Clone)]
pub struct MessageSalience {
    /// Proxy-token size at/above which a message is considered a "large blob" and
    /// pushed toward the low-value end (big `codegraph_explore`/diff dumps).
    pub large_blob_tokens: usize,
}

impl Default for MessageSalience {
    fn default() -> Self {
        // ~2K proxy tokens ≈ an 8K-char tool dump: clearly bulk, not a decision.
        Self {
            large_blob_tokens: 2_000,
        }
    }
}

impl Salience<Message> for MessageSalience {
    /// Score a message in `[0.0, 1.0]`. `occurrences` is ignored (frequency is a
    /// Step-2c follow-up — it slides across turns and would break cache stability).
    fn score(&self, m: &Message, _occurrences: u32) -> f32 {
        // Role prior: a user directive or an assistant decision is worth keeping
        // sharp; a raw tool result is bulk that compresses cheaply. (Assistant
        // messages are never compacted by the caller, but score them high for
        // completeness / any future caller.)
        let role_score = match m.role {
            Role::User => 0.9,
            Role::Assistant => 0.8,
            Role::System => 0.7,
            Role::Tool => 0.3,
        };

        // Size penalty: a large blob is almost always low-signal bulk regardless of
        // role, so scale the score down as it grows past the large-blob threshold.
        let tokens = proxy_tokens(&m.content);
        let size_factor = if self.large_blob_tokens == 0 {
            1.0
        } else {
            // 1.0 at/below the threshold, decaying toward 0.25 for very large blobs.
            let over = tokens as f32 / self.large_blob_tokens as f32;
            (1.0 / over.max(1.0)).clamp(0.25, 1.0)
        };

        (role_score * size_factor).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: Role, content: &str) -> Message {
        Message {
            id: "m".into(),
            session_id: "s".into(),
            role,
            content: content.into(),
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
    fn user_directive_outscores_tool_dump_of_same_size() {
        let s = MessageSalience::default();
        let text = "x".repeat(400); // small, same size both
        assert!(
            s.score(&msg(Role::User, &text), 0) > s.score(&msg(Role::Tool, &text), 0),
            "a user directive must outrank a tool result of equal size"
        );
    }

    #[test]
    fn large_blob_scores_lower_than_small_message_same_role() {
        let s = MessageSalience::default();
        let small = s.score(&msg(Role::Tool, "ok"), 0);
        let big = s.score(&msg(Role::Tool, &"x ".repeat(20_000)), 0);
        assert!(
            big < small,
            "a large tool dump must score below a small one"
        );
    }

    #[test]
    fn score_is_stable_regardless_of_occurrences() {
        // Cache-stability: occurrences must not change the score (frequency deferred).
        let s = MessageSalience::default();
        let m = msg(Role::User, "keep this");
        assert_eq!(s.score(&m, 0), s.score(&m, 99));
    }

    #[test]
    fn score_is_bounded() {
        let s = MessageSalience::default();
        for m in [
            msg(Role::User, ""),
            msg(Role::Tool, &"x".repeat(1_000_000)),
            msg(Role::System, "sys"),
        ] {
            let v = s.score(&m, 0);
            assert!((0.0..=1.0).contains(&v), "score out of range: {v}");
        }
    }
}
