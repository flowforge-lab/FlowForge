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
//! - [`supports_vision_in`]: provider-scoped (the `provider` field is consulted;
//!   `None` matches any) and fail-closed -- `false` unless some matching rule
//!   carries `supports_vision: true`. Unlike the window lookup it is an OR over
//!   all matching rules, not first-match-wins (#466).

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
    /// Provider scope for the rule (#466). The same model substring can mean
    /// different things per provider (`-vl` is vision on SiliconFlow, nothing on
    /// Bedrock), so vision rules are keyed on this; `None` means "any provider".
    /// Consulted by [`supports_vision_in`]; ignored by the provider-agnostic
    /// [`context_window_in`].
    #[serde(default)]
    provider: Option<ProviderKind>,
    /// Whether the model accepts image/document input (#466). Backs the vision
    /// gate via [`supports_vision_in`], fail-closed: absent/`None` means "unknown"
    /// and is treated as `false`. A rule must set this to `true` to grant vision.
    #[serde(default)]
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

/// Provider-scoped, fail-closed vision capability lookup over a rule slice (#466).
/// Returns `true` only when some rule matches *and* carries `supports_vision:
/// true`; a rule's `provider` must be `None` (any) or equal `kind`. This is an OR
/// over all matching rules (not first-match-wins): the question is whether *any*
/// rule grants vision for `(kind, model)`. Window-only rules (`supports_vision:
/// None`) never grant or block vision, so they cannot interfere. Conservative by
/// design -- an unknown model returns `false`, and the FE attach gate and provider
/// safety strip both fail closed on `false`.
pub fn supports_vision_in(rules: &[ModelSpec], kind: ProviderKind, model: &str) -> bool {
    let m = model.to_lowercase();
    rules.iter().any(|r| {
        r.supports_vision == Some(true)
            && r.provider.is_none_or(|p| p == kind)
            && m.contains(&r.pattern.to_lowercase())
    })
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

    /// Vision fields are consulted by `supports_vision_in` but never by the
    /// provider-agnostic window lookup.
    #[test]
    fn vision_fields_are_ignored_by_window_lookup() {
        let specs = parse_specs(
            r#"{ "rules": [
                { "match": "bar", "context_window": 8192,
                  "provider": "openai", "supports_vision": true }
            ] }"#,
        )
        .unwrap();
        assert_eq!(context_window_in(&specs.rules, "bar-v1"), 8192);
    }

    // ---- #466: provider-scoped, fail-closed vision lookup ----

    #[test]
    fn supports_vision_covers_known_families_per_provider() {
        let r = bundled_rules();
        // Bedrock: modern Claude + named Mythos/Fable.
        for m in [
            "us.anthropic.claude-opus-4-8",
            "us.anthropic.claude-sonnet-4-6",
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            "us.anthropic.claude-3-opus-20240229-v1:0",
            "claude-mythos-5",
            "claude-fable-5",
        ] {
            assert!(
                supports_vision_in(r, ProviderKind::Bedrock, m),
                "bedrock {m}"
            );
        }
        // OpenAI vision families.
        for m in [
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4-turbo",
            "gpt-5",
            "o1",
            "o3-mini",
        ] {
            assert!(supports_vision_in(r, ProviderKind::OpenAi, m), "openai {m}");
        }
        // Ollama vision tags.
        for m in [
            "llava:7b",
            "llama3.2-vision",
            "moondream",
            "qwen2-vl:7b",
            "bakllava",
        ] {
            assert!(supports_vision_in(r, ProviderKind::Ollama, m), "ollama {m}");
        }
        // SiliconFlow VL / 4V suffixes.
        for m in ["Qwen/Qwen2-VL-7B-Instruct", "zai-org/GLM-4V-9B"] {
            assert!(
                supports_vision_in(r, ProviderKind::SiliconFlow, m),
                "siliconflow {m}"
            );
        }
        // Text-only stays text-only, and CandleVllm has no rules (always false).
        for (k, m) in [
            (ProviderKind::Ollama, "llama3.2"),
            (ProviderKind::OpenAi, "gpt-3.5-turbo"),
            (ProviderKind::Bedrock, "meta.llama3-70b-instruct-v1:0"),
            (ProviderKind::SiliconFlow, "deepseek-ai/DeepSeek-V3"),
            (ProviderKind::CandleVllm, "anything"),
        ] {
            assert!(
                !supports_vision_in(r, k, m),
                "expected text-only: {k:?} {m}"
            );
        }
    }

    #[test]
    fn vision_is_provider_scoped() {
        let r = bundled_rules();
        // `-vl` is a SiliconFlow vision suffix; the same substring on Bedrock or
        // OpenAI must not grant vision (the rule's `provider` is scoped).
        assert!(supports_vision_in(
            r,
            ProviderKind::SiliconFlow,
            "qwen2-vl-7b"
        ));
        assert!(!supports_vision_in(
            r,
            ProviderKind::Bedrock,
            "some-vl-model"
        ));
        assert!(!supports_vision_in(
            r,
            ProviderKind::OpenAi,
            "some-vl-model"
        ));
        // OpenAI `o1`/`o3` reasoning vision must not leak to other providers.
        assert!(supports_vision_in(r, ProviderKind::OpenAi, "o1-mini"));
        assert!(!supports_vision_in(r, ProviderKind::Ollama, "o1-mini"));
    }

    #[test]
    fn window_only_rules_do_not_grant_vision() {
        // The generic `glm` / `deepseek` window rules carry no `supports_vision`,
        // so they must never un-gate vision regardless of provider.
        let r = bundled_rules();
        assert!(!supports_vision_in(
            r,
            ProviderKind::SiliconFlow,
            "zai-org/GLM-4.5"
        ));
        assert!(!supports_vision_in(
            r,
            ProviderKind::SiliconFlow,
            "deepseek-ai/DeepSeek-V3"
        ));
    }

    #[test]
    fn vision_fail_closed_and_supports_vision_false_does_not_grant() {
        // A matching rule must carry `supports_vision: true`; an explicit `false`
        // or an absent field never grants. Unknown model -> false.
        let specs = parse_specs(
            r#"{ "rules": [
                { "match": "x1", "provider": "openai", "supports_vision": false },
                { "match": "x2", "provider": "openai" }
            ] }"#,
        )
        .unwrap();
        assert!(!supports_vision_in(
            &specs.rules,
            ProviderKind::OpenAi,
            "x1-model"
        ));
        assert!(!supports_vision_in(
            &specs.rules,
            ProviderKind::OpenAi,
            "x2-model"
        ));
        assert!(!supports_vision_in(
            &specs.rules,
            ProviderKind::OpenAi,
            "unknown"
        ));
    }

    /// Migration parity guard (#466): the data-driven lookup must reproduce the
    /// old hardcoded `match` arms across representative + adversarial ids. The
    /// closure is the pre-migration logic, frozen here so a future data edit that
    /// silently changes a known family fails loudly.
    ///
    /// Known divergence (documented, accepted): the old OpenAI arm used
    /// `starts_with("o1"|"o3")` while rules match by substring, so an id that
    /// merely *contains* `o1`/`o3` now matches on an OpenAI connection. No real
    /// OpenAI vision id is affected; the cross-cutting word-boundary matcher is
    /// tracked separately. The adversarial `proto1` case below pins this.
    #[test]
    fn data_driven_matches_legacy_arms() {
        fn legacy(kind: ProviderKind, model: &str) -> bool {
            let m = model.to_ascii_lowercase();
            match kind {
                ProviderKind::Bedrock => {
                    m.contains("claude-3")
                        || m.contains("claude-opus-4")
                        || m.contains("claude-sonnet-4")
                        || m.contains("claude-haiku-4")
                        || m.contains("mythos")
                        || m.contains("fable")
                }
                ProviderKind::OpenAi => {
                    m.contains("gpt-4o")
                        || m.contains("gpt-4.1")
                        || m.contains("gpt-4-turbo")
                        || m.contains("gpt-5")
                        || m.starts_with("o1")
                        || m.starts_with("o3")
                }
                ProviderKind::Ollama => {
                    m.contains("llava")
                        || m.contains("-vision")
                        || m.contains("moondream")
                        || m.contains("bakllava")
                        || m.contains("qwen2-vl")
                }
                ProviderKind::SiliconFlow => m.contains("-vl") || m.contains("-4v"),
                ProviderKind::CandleVllm => false,
            }
        }
        let r = bundled_rules();
        let kinds = [
            ProviderKind::Bedrock,
            ProviderKind::OpenAi,
            ProviderKind::Ollama,
            ProviderKind::SiliconFlow,
            ProviderKind::CandleVllm,
        ];
        let models = [
            "gpt-4o",
            "gpt-4o-mini",
            "gpt-4.1",
            "gpt-4-turbo",
            "gpt-5",
            "o1",
            "o3-mini",
            "gpt-3.5-turbo",
            "us.anthropic.claude-opus-4-8",
            "claude-3-5-sonnet",
            "claude-mythos-5",
            "claude-fable-5",
            "meta.llama3-70b",
            "llava:7b",
            "llama3.2-vision",
            "moondream",
            "qwen2-vl:7b",
            "llama3.2",
            "Qwen/Qwen2-VL-7B",
            "zai-org/GLM-4V-9B",
            "deepseek-ai/DeepSeek-V3",
            "anything",
            "some-local-7b",
        ];
        for &k in &kinds {
            for &model in &models {
                assert_eq!(
                    supports_vision_in(r, k, model),
                    legacy(k, model),
                    "parity mismatch for {k:?} / {model}"
                );
            }
        }
        // Documented divergence: substring vs the old `starts_with` for o1/o3.
        assert!(!legacy(ProviderKind::OpenAi, "proto1"));
        assert!(supports_vision_in(r, ProviderKind::OpenAi, "proto1"));
    }
}
