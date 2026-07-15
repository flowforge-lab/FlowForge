//! Token estimation backed by [`tokenx-rs`] -- a zero-dependency single-pass
//! character scanner with ~96% accuracy against tiktoken cl100k_base. No
//! vocabulary files, no initialization cost, no binary size impact.
//!
//! Sufficient for compaction triggers, output budgeting, and observability
//! gauges. Exact BPE counting (tiktoken-rs) can be layered in later if
//! token-level trimming requires sub-percent accuracy (#933).

/// Estimate the token count of `text`. Model-agnostic -- the heuristic works
/// uniformly across all BPE-trained LLMs (OpenAI, Claude, Qwen, Llama,
/// DeepSeek, GLM) with ~96% accuracy.
pub fn count_tokens(text: &str) -> usize {
    tokenx_rs::estimate_token_count(text)
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
}
