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
/// The OpenAI `o1`/`o3` rules are `match_kind: prefix` (#473), reproducing
/// the old `starts_with` arm exactly, so there is no longer a divergence: an
/// id that merely *contains* `o1`/`o3` (e.g. `proto1`, `foo-o1`) does not
/// match, just as the legacy arm required.
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
        // #473 adversarial: contain "o1"/"o3" but are not prefixed by them.
        "proto1",
        "foo-o1",
        "gpt-4o3",
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
}

/// #473: anchoring the `o1`/`o3` rules as `prefix` fixes the substring trap
/// consistently across BOTH the window and the vision lookup -- an id that
/// merely contains `o1`/`o3` falls through to the defaults, while a real
/// reasoning id (prefixed) still matches.
#[test]
fn o1_o3_prefix_rules_do_not_match_substring_ids() {
    let r = bundled_rules();
    for trap in ["proto1", "foo-o1", "macro3"] {
        assert_eq!(
            context_window_in(r, trap),
            DEFAULT_CONTEXT_WINDOW_TOKENS,
            "{trap} must not pick up the o1/o3 window rule"
        );
        assert!(
            !supports_vision_in(r, ProviderKind::OpenAi, trap),
            "{trap} must not pick up the o1/o3 vision rule"
        );
    }
    for real in ["o1", "o1-mini", "o1-preview", "o3", "o3-mini"] {
        assert_eq!(
            context_window_in(r, real),
            128_000,
            "{real} should match the o1/o3 window rule"
        );
        assert!(
            supports_vision_in(r, ProviderKind::OpenAi, real),
            "{real} should match the o1/o3 vision rule"
        );
    }
}

/// #473: the shared matcher honors `match_kind` for both `contains` (default)
/// and `prefix`, parsed from JSON.
#[test]
fn match_kind_contains_and_prefix() {
    let specs = parse_specs(
        r#"{ "rules": [
            { "match": "foo", "context_window": 111 },
            { "match": "bar", "match_kind": "prefix", "context_window": 222 }
        ] }"#,
    )
    .unwrap();
    let r = &specs.rules;
    // contains: matches anywhere.
    assert_eq!(context_window_in(r, "x-foo-y"), 111);
    // prefix: only at the start.
    assert_eq!(context_window_in(r, "bar-1"), 222);
    assert_eq!(context_window_in(r, "x-bar"), DEFAULT_CONTEXT_WINDOW_TOKENS);
}
