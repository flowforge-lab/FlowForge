//! Data-driven model capability specs (#457).
//!
//! A single ordered rule set describes per-model properties — today the context
//! window, with room for vision support and other quirks — so adding or
//! correcting one is a data edit, not a code change. A flat per-vendor rule
//! cannot track reality: served context windows span 96K–1M *within* a vendor
//! family (e.g. `GLM-4.5-Air` 98,304 vs `GLM-5.2` 1,048,576) and drift with
//! every point release, so that churn belongs in data.
//!
//! This module owns the **schema, the bundled defaults, and the pure lookups**.
//! It does no I/O: the optional user-override layer (reading a config-dir file
//! and merging it ahead of the bundled rules) lives in `ff-llm`, which feeds the
//! merged rule slice back into [`context_window_in`]. Keeping the pure layer here
//! lets capability lookups in *other* crates (e.g. the vision gate in
//! [`crate::provider`]) share one schema and one bundled file without a reverse
//! dependency on `ff-llm`.
//!
//! ## Matching
//! `match` is a case-insensitive substring of the model id; rules are evaluated
//! **first-match-wins**, so the list is ordered most-specific-first (e.g.
//! `glm-4.5-air` before `glm`). Per-field absent semantics differ by capability
//! and are documented on each lookup:
//! - [`context_window_in`]: the first matching rule that carries a
//!   `context_window` wins; unknowns fall through to
//!   [`DEFAULT_CONTEXT_WINDOW_TOKENS`]. Provider-agnostic — the window is a
//!   property of the model, not the transport, so `provider` is ignored here.
//! - `supports_vision` / `provider` are **reserved** for the forthcoming vision
//!   migration (provider-scoped, fail-closed `false`); they are parsed and
//!   carried but not yet consulted.

use serde::Deserialize;

use crate::ProviderKind;

/// Conservative fallback window for a model with no matching rule. Small on
/// purpose: undersizing throttles a capable model but never overflows it, while
/// oversizing lets a request blow past the real cap before compaction engages.
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 32_000;

/// Bundled defaults, seeded from live SiliconFlow probes (2026-06-24) plus
/// official Anthropic/OpenAI windows. Trusted input: a parse failure here is a
/// build/CI defect, caught by [`tests::bundled_rules_parse_and_lookup`].
const DEFAULT_JSON: &str = include_str!("model-specs.default.json");

/// One capability rule. `match` is a case-insensitive substring of the model id.
/// Capability fields are optional so a rule can describe only what it knows; new
/// fields (`max_output`, pricing, …) can be added later with `#[serde(default)]`
/// without breaking files written against an older schema (serde also ignores
/// unknown fields on the way in).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpec {
    #[serde(rename = "match")]
    pattern: String,
    /// Context window in tokens. Absent on a rule that describes only other
    /// capabilities (e.g. a future vision-only rule).
    #[serde(default)]
    context_window: Option<u64>,
    /// Reserved (#457): provider-scoped matching for the vision migration. The
    /// same model substring can mean different things per provider (`-vl` is
    /// vision on SiliconFlow, nothing on Bedrock), so vision rules will be keyed
    /// on this; `None` means "any provider". Not consulted yet.
    #[serde(default)]
    #[allow(dead_code)]
    provider: Option<ProviderKind>,
    /// Reserved (#457): whether the model accepts image/document input. Will
    /// back the vision gate (fail-closed: absent/`None` means "unknown" ->
    /// treated as `false`). Not consulted yet.
    #[serde(default)]
    #[allow(dead_code)]
    supports_vision: Option<bool>,
}

/// A parsed rule set (bundled defaults or a user override).
#[derive(Debug, Clone, Deserialize)]
pub struct ModelSpecs {
    pub rules: Vec<ModelSpec>,
}

/// Parse a rule set from JSON. Used by the `ff-llm` override layer to read the
/// user's on-disk file into the shared schema.
pub fn parse_specs(json: &str) -> Result<ModelSpecs, serde_json::Error> {
    serde_json::from_str(json)
}

/// The compiled-in default rules, parsed once.
pub fn bundled_rules() -> &'static [ModelSpec] {
    use std::sync::OnceLock;
    static BUNDLED: OnceLock<Vec<ModelSpec>> = OnceLock::new();
    BUNDLED.get_or_init(|| {
        parse_specs(DEFAULT_JSON)
            .expect("bundled model-specs.default.json is malformed")
            .rules
    })
}

/// First-match-wins context-window lookup over an ordered rule slice. Returns the
/// `context_window` of the first matching rule that carries one; rules without a
/// window (capability-only) are skipped so they cannot shadow a later windowed
/// rule. Provider-agnostic by design. Unknowns fall through to
/// [`DEFAULT_CONTEXT_WINDOW_TOKENS`].
pub fn context_window_in(rules: &[ModelSpec], model: &str) -> u64 {
    let m = model.to_lowercase();
    rules
        .iter()
        .filter(|r| m.contains(&r.pattern.to_lowercase()))
        .find_map(|r| r.context_window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_rules_parse_and_lookup() {
        let rules = bundled_rules();
        assert!(!rules.is_empty());
        // Family substrings (case-insensitive), not exact ids, so point releases inherit.
        assert_eq!(context_window_in(rules, "zai-org/GLM-5.2"), 1_048_576);
        assert_eq!(context_window_in(rules, "zai-org/GLM-5"), 202_752);
        assert_eq!(context_window_in(rules, "zai-org/GLM-4.5"), 131_072);
        assert_eq!(
            context_window_in(rules, "deepseek-ai/DeepSeek-V4-Pro"),
            1_000_000
        );
        assert_eq!(
            context_window_in(rules, "deepseek-ai/DeepSeek-V3.2"),
            163_840
        );
        assert_eq!(
            context_window_in(rules, "moonshotai/Kimi-K2.7-Code"),
            262_144
        );
        assert_eq!(context_window_in(rules, "MiniMaxAI/MiniMax-M3"), 700_000);
        assert_eq!(context_window_in(rules, "MiniMaxAI/MiniMax-M2.5"), 196_608);
        assert_eq!(context_window_in(rules, "anthropic.claude-opus-4"), 200_000);
        assert_eq!(context_window_in(rules, "gpt-4o-mini"), 128_000);
        assert_eq!(
            context_window_in(rules, "some-local-7b"),
            DEFAULT_CONTEXT_WINDOW_TOKENS
        );
    }

    /// GLM-4.5-Air must NOT inherit the generic `glm` window: its served cap
    /// (98,304) is below the budget the old flat 128K rule produced, so the more
    /// specific rule must precede the generic one.
    #[test]
    fn glm_4_5_air_more_specific_rule_wins() {
        let rules = bundled_rules();
        assert_eq!(context_window_in(rules, "zai-org/GLM-4.5-Air"), 98_304);
        assert_ne!(context_window_in(rules, "zai-org/GLM-4.5-Air"), 131_072);
    }

    /// Regression guard (#457): no bundled rule may report a window larger than
    /// the cap the provider actually serves, or a budget computed from it never
    /// triggers compaction in time.
    #[test]
    fn no_bundled_window_exceeds_probed_served_cap() {
        let rules = bundled_rules();
        let probed: &[(&str, u64)] = &[
            ("zai-org/GLM-5.2", 1_048_576),
            ("zai-org/GLM-5.1", 202_752),
            ("zai-org/GLM-5", 202_752),
            ("zai-org/GLM-5V-Turbo", 202_752),
            ("zai-org/GLM-4.5-Air", 98_304),
            ("deepseek-ai/DeepSeek-V4-Pro", 1_000_000),
            ("deepseek-ai/DeepSeek-V4-Flash", 1_048_576),
            ("deepseek-ai/DeepSeek-V3.2", 163_840),
            ("moonshotai/Kimi-K2.7-Code", 262_144),
            ("MiniMaxAI/MiniMax-M3", 700_000),
            ("MiniMaxAI/MiniMax-M2.5", 196_608),
        ];
        for (model, served) in probed {
            assert!(
                context_window_in(rules, model) <= *served,
                "{model}: reported window {} exceeds served cap {served}",
                context_window_in(rules, model),
            );
        }
    }

    /// A capability-only rule (no `context_window`) must not shadow a later rule
    /// that carries the window for the same family.
    #[test]
    fn windowless_rule_does_not_shadow_a_windowed_one() {
        let specs = parse_specs(
            r#"{ "rules": [
                { "match": "foo", "supports_vision": true },
                { "match": "foo", "context_window": 4096 }
            ] }"#,
        )
        .unwrap();
        assert_eq!(context_window_in(&specs.rules, "foo-7b"), 4096);
    }

    /// Reserved fields parse without being consulted by the window lookup.
    #[test]
    fn reserved_fields_parse_and_are_ignored_by_window_lookup() {
        let specs = parse_specs(
            r#"{ "rules": [
                { "match": "bar", "context_window": 8192,
                  "provider": "openai", "supports_vision": true }
            ] }"#,
        )
        .unwrap();
        assert_eq!(context_window_in(&specs.rules, "bar-v1"), 8192);
    }
}
