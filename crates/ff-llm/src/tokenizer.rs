//! Token estimation backed by [`tokenx-rs`] -- a zero-dependency single-pass
//! character scanner with ~96% accuracy against tiktoken cl100k_base. No
//! vocabulary files, no initialization cost, no binary size impact.
//!
//! Sufficient for compaction triggers, output budgeting, and observability
//! gauges. Exact BPE counting (tiktoken-rs) can be layered in later if
//! token-level trimming requires sub-percent accuracy (#933).
//!
//! ## Code-identifier tuning
//!
//! Stock tokenx scores a word segment as one-token-per-char whenever it
//! contains a non-alphanumeric char (see its `score_word` fallback). That
//! makes `snake_case`/`SCREAMING_CASE` identifiers -- ubiquitous in the
//! code-heavy prompts this app sends -- overcount by ~5x (`process_items`
//! estimates as 13 tokens; tiktoken counts ~2). tokenx exposes a second
//! path: any segment with a matched `LanguageConfig` is scored as
//! `ceil(byte_len / chars_per_token)` instead. We prepend a rule matching
//! `_` at ~4 chars/token, routing identifiers through that path
//! (`process_items` -> ceil(13/4) = 4). `EstimationOptions::default()` still
//! carries tokenx's built-in German/French/Spanish rules; `_` and those
//! diacritics are disjoint char sets, so ordering is immaterial to
//! correctness.

use std::sync::LazyLock;
use tokenx_rs::{estimate_token_count_with_options, EstimationOptions, LanguageConfig};

/// Built once, not per call: [`count_tokens`] runs on the compaction hot path
/// (`ContextPressureEstimator::assess` fires every round-trip and every turn
/// end), so we avoid reallocating the `language_configs` vec each invocation.
static OPTS: LazyLock<EstimationOptions> = LazyLock::new(|| {
    let mut opts = EstimationOptions::default();
    opts.language_configs.insert(
        0,
        LanguageConfig {
            matcher: |c| c == '_',
            chars_per_token: 4.0,
        },
    );
    opts
});

/// Estimate the token count of `text`. Model-agnostic -- the heuristic works
/// uniformly across all BPE-trained LLMs (OpenAI, Claude, Qwen, Llama,
/// DeepSeek, GLM) with ~96% accuracy.
pub fn count_tokens(text: &str) -> usize {
    estimate_token_count_with_options(text, &OPTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimates_known_inputs() {
        // Single repeated char: a long alphanumeric "word".
        let x100k = "x".repeat(100_000);
        let t = count_tokens(&x100k);
        eprintln!("100k x = {t} tokens");
        // Should be roughly 100_000 / 5-6 ~ 17k-20k
        assert!(t > 10_000 && t < 30_000, "100k x got {t}");

        // English prose
        let sentence = "The quick brown fox jumps over the lazy dog. ";
        let text = sentence.repeat(2000);
        let t2 = count_tokens(&text);
        eprintln!("{} chars English = {t2} tokens", text.len());
        // ~90k chars / ~4 chars/token ~ 22k tokens
        assert!(t2 > 15_000 && t2 < 30_000, "English got {t2}");

        // Empty
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn code_identifiers_not_overcounted() {
        // snake_case / SCREAMING_CASE previously hit tokenx's per-char
        // fallback (process_items = 13, DEFAULT_CHARS_PER_TOKEN = 23). The
        // `_` LanguageConfig routes them through ceil(byte_len / 4.0).
        let items = count_tokens("process_items");
        assert!(items <= 6, "process_items got {items}, expected <= 6");

        let screaming = count_tokens("DEFAULT_CHARS_PER_TOKEN");
        assert!(
            screaming <= 8,
            "DEFAULT_CHARS_PER_TOKEN got {screaming}, expected <= 8"
        );

        // camelCase is all-alphanumeric, already on the good path -- unaffected.
        let camel = count_tokens("handleClick");
        assert!(camel <= 3, "handleClick got {camel}, expected <= 3");
    }
}
