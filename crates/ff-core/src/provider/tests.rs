use super::*;

#[test]
fn default_is_local_candle_vllm() {
    let cfg = ProviderConfig::default();
    assert_eq!(cfg.kind, ProviderKind::CandleVllm);
    assert_eq!(cfg.base_url, None);
    assert!(!cfg.has_key);
}

#[test]
fn is_local_true_for_local_kinds_only() {
    assert!(ProviderKind::CandleVllm.is_local());
    assert!(ProviderKind::Ollama.is_local());
    assert!(!ProviderKind::Bedrock.is_local());
    assert!(!ProviderKind::OpenAi.is_local());
    assert!(!ProviderKind::SiliconFlow.is_local());
}

#[test]
fn default_thinking_off_for_local_on_for_hosted() {
    assert!(!ProviderKind::CandleVllm.default_thinking());
    assert!(!ProviderKind::Ollama.default_thinking());
    assert!(ProviderKind::Bedrock.default_thinking());
    assert!(ProviderKind::OpenAi.default_thinking());
    assert!(ProviderKind::SiliconFlow.default_thinking());
}

#[test]
fn default_config_thinking_off_for_local() {
    assert!(!ProviderConfig::default().thinking);
}

#[test]
fn default_registry_seeds_local_thinking_off() {
    let reg = ProviderRegistry::default();
    assert_eq!(reg.schema_version, REGISTRY_SCHEMA_VERSION);
    for conn in &reg.connections {
        assert!(conn.kind.is_local());
        assert!(
            !conn.thinking,
            "seeded local connection {} should default thinking off",
            conn.id
        );
    }
}

#[test]
fn migrate_flips_existing_local_thinking_off_and_stamps_version() {
    let mut reg = ProviderRegistry {
        active: "ollama".to_string(),
        connections: vec![blank_conn("Ollama", None, ProviderKind::Ollama)],
        schema_version: 0,
    };
    reg.connections[0].thinking = true;
    reg.migrate();
    assert_eq!(reg.schema_version, REGISTRY_SCHEMA_VERSION);
    assert!(!reg.connections[0].thinking);
}

#[test]
fn migrate_leaves_hosted_thinking_untouched() {
    let mut reg = ProviderRegistry {
        active: "bedrock".to_string(),
        connections: vec![blank_conn("AWS Bedrock", None, ProviderKind::Bedrock)],
        schema_version: 0,
    };
    reg.connections[0].thinking = true;
    reg.migrate();
    assert!(
        reg.connections[0].thinking,
        "hosted connection thinking must survive migration"
    );
}

#[test]
fn migrate_is_run_once_reenabled_local_thinking_survives() {
    let mut reg = ProviderRegistry {
        active: "ollama".to_string(),
        connections: vec![blank_conn("Ollama", None, ProviderKind::Ollama)],
        schema_version: REGISTRY_SCHEMA_VERSION,
    };
    reg.connections[0].thinking = true;
    reg.migrate();
    assert!(
        reg.connections[0].thinking,
        "already-migrated registry must not re-flip a re-enabled local connection"
    );
}

#[test]
fn default_config_enables_warmup() {
    assert!(ProviderConfig::default().warmup_enabled);
}

#[test]
fn legacy_config_without_warmup_defaults_enabled() {
    // A pre-#61 provider.json has no `warmupEnabled` key; it must load as true
    // so existing installs keep today's behavior (no regression).
    let json = r#"{"kind":"ollama","model":"llama3.2","hasKey":false,"thinking":true}"#;
    let cfg: ProviderConfig = serde_json::from_str(json).unwrap();
    assert!(cfg.warmup_enabled);
}

#[test]
fn siliconflow_kind_defaults_global_endpoint_and_slug() {
    assert_eq!(
        ProviderKind::SiliconFlow.default_base_url(),
        "https://api.siliconflow.com/v1"
    );
    assert_eq!(ProviderKind::SiliconFlow.slug(), "siliconflow");
}

#[test]
fn siliconflow_kind_serializes_camel_case() {
    let json = serde_json::to_string(&ProviderKind::SiliconFlow).unwrap();
    assert_eq!(json, "\"siliconFlow\"");
}

#[test]
fn resolved_base_url_falls_back_to_kind_default() {
    let cfg = ProviderConfig {
        kind: ProviderKind::Ollama,
        base_url: None,
        ..ProviderConfig::default()
    };
    assert_eq!(cfg.resolved_base_url(), "http://localhost:11434");
}

#[test]
fn resolved_base_url_prefers_override() {
    let cfg = ProviderConfig {
        base_url: Some("http://example:9000/v1".into()),
        ..ProviderConfig::default()
    };
    assert_eq!(cfg.resolved_base_url(), "http://example:9000/v1");
}

#[test]
fn config_round_trips_through_json_without_secrets() {
    let cfg = ProviderConfig::default();
    let json = serde_json::to_string(&cfg).unwrap();
    assert!(!json.contains("baseUrl"), "None base_url is skipped");
    assert!(json.contains("hasKey"));
    let back: ProviderConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(cfg, back);
}

#[test]
fn legacy_config_without_visibility_defaults_to_all() {
    // #549: a pre-#549 provider.json carries no `reasoningVisibility`; it must
    // load as `All` so the natural final answer shows a Thought block.
    let json = r#"{"kind":"ollama","model":"llama3","hasKey":false,"thinking":true}"#;
    let cfg: ProviderConfig = serde_json::from_str(json).unwrap();
    assert_eq!(cfg.reasoning_visibility, ReasoningVisibility::All);
    assert_eq!(ReasoningVisibility::default(), ReasoningVisibility::All);
}

#[test]
fn default_registry_has_two_local_connections_candle_active() {
    let reg = ProviderRegistry::default();
    assert_eq!(reg.connections.len(), 2);
    assert_eq!(reg.active, "candle-vllm");
    let active = reg.active_connection().expect("active resolves");
    assert_eq!(active.kind, ProviderKind::CandleVllm);
    assert!(reg
        .connections
        .iter()
        .any(|c| c.kind == ProviderKind::Ollama));
}

// A frozen corpus of historically-persisted `provider-registry.json` shapes.
// Every one MUST strict-parse forever and keep its own `active` (never fall
// back to the Candle default). Adding a required field without `#[serde(default)]`
// breaks this test loudly at CI time -- the signal to add the default rather
// than silently reintroduce the wipe (#811).
const HISTORICAL_REGISTRIES: &[(&str, &str)] = &[
    // Pre-versioning minimal shape: only the required fields, no schemaVersion.
    (
        "pre-versioning minimal Bedrock",
        r#"{"connections":[{"id":"bedrock-opus","kind":"bedrock","displayName":"AWS Bedrock","model":"global.anthropic.claude-opus-4-8","hasKey":false}],"active":"bedrock-opus"}"#,
    ),
    // Fuller Bedrock shape with profile auth + region (the config that broke
    // under an older build, sanitized).
    (
        "Bedrock with profile auth + region",
        r#"{"schemaVersion":1,"active":"bedrock-opus","connections":[{"id":"bedrock-opus","kind":"bedrock","displayName":"AWS Bedrock","model":"global.anthropic.claude-opus-4-8","hasKey":false,"thinking":true,"region":"us-east-2","authMode":"profile","awsProfile":"bedrock-profile"}]}"#,
    ),
];

#[test]
fn historical_registry_shapes_still_strict_parse() {
    for (label, raw) in HISTORICAL_REGISTRIES {
        let reg: ProviderRegistry = serde_json::from_str(raw)
            .unwrap_or_else(|e| panic!("{label} must still strict-parse: {e}"));
        assert_eq!(
            reg.active, "bedrock-opus",
            "{label}: active must be preserved, not reset to the Candle default"
        );
        assert_eq!(reg.connections[0].kind, ProviderKind::Bedrock, "{label}");
    }
}

#[test]
fn parse_lenient_accepts_valid_registry() {
    for (label, raw) in HISTORICAL_REGISTRIES {
        let reg = ProviderRegistry::parse_lenient(raw)
            .unwrap_or_else(|| panic!("{label}: lenient parse should accept a valid registry"));
        assert_eq!(reg.active, "bedrock-opus", "{label}");
    }
}

#[test]
fn parse_lenient_salvages_good_connection_beside_bad() {
    // Two connections; the first carries an unknown `kind` this build cannot
    // deserialize. The good one survives, and the recorded active (the bad one)
    // falls back to the surviving connection rather than wiping to default.
    let raw = r#"{"active":"future-gemini","connections":[
        {"id":"future-gemini","kind":"gemini","displayName":"Gemini","model":"g","hasKey":true},
        {"id":"bedrock-opus","kind":"bedrock","displayName":"AWS Bedrock","model":"global.anthropic.claude-opus-4-8","hasKey":false}
    ]}"#;
    let reg = ProviderRegistry::parse_lenient(raw).expect("one good connection must be salvaged");
    assert_eq!(reg.connections.len(), 1);
    assert_eq!(reg.connections[0].id, "bedrock-opus");
    assert_eq!(
        reg.active, "bedrock-opus",
        "active must fall back to a surviving connection, not the Candle default"
    );
}

#[test]
fn parse_lenient_preserves_recorded_active_when_it_survives() {
    let raw = r#"{"active":"bedrock-opus","connections":[
        {"id":"future-gemini","kind":"gemini","displayName":"Gemini","model":"g","hasKey":true},
        {"id":"bedrock-opus","kind":"bedrock","displayName":"AWS Bedrock","model":"m","hasKey":false}
    ]}"#;
    let reg = ProviderRegistry::parse_lenient(raw).expect("salvage");
    assert_eq!(reg.active, "bedrock-opus");
}

#[test]
fn parse_lenient_returns_none_when_nothing_salvageable() {
    let raw = r#"{"active":"x","connections":[
        {"id":"a","kind":"gemini","displayName":"A","model":"m","hasKey":false},
        {"id":"b","kind":"mistral","displayName":"B","model":"m","hasKey":false}
    ]}"#;
    assert!(
        ProviderRegistry::parse_lenient(raw).is_none(),
        "no salvageable connection must yield None so the caller quarantines + defaults"
    );
}

#[test]
fn parse_lenient_returns_none_on_garbage() {
    assert!(ProviderRegistry::parse_lenient("not json at all").is_none());
    assert!(ProviderRegistry::parse_lenient("[]").is_none());
    assert!(ProviderRegistry::parse_lenient("{}").is_none());
}

fn blank_conn(display: &str, vendor: Option<&str>, kind: ProviderKind) -> ProviderConnection {
    ProviderConnection {
        id: String::new(),
        kind,
        display_name: display.to_string(),
        vendor: vendor.map(str::to_string),
        base_url: None,
        model: "m".to_string(),
        has_key: false,
        secret_missing: false,
        thinking: true,
        reasoning_effort: ReasoningEffort::default(),
        reasoning_visibility: ReasoningVisibility::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    }
}

#[test]
fn derive_id_dedupes_against_seeded_connection() {
    let reg = ProviderRegistry::default();
    // "ollama" is already seeded, so a new "Ollama" derives ollama-2.
    let id = reg.derive_id(&blank_conn("Ollama", None, ProviderKind::Ollama));
    assert_eq!(id, "ollama-2");
}

#[test]
fn derive_id_prefers_vendor_then_display_then_kind() {
    let reg = ProviderRegistry {
        connections: vec![],
        active: String::new(),
        schema_version: REGISTRY_SCHEMA_VERSION,
    };
    assert_eq!(
        reg.derive_id(&blank_conn(
            "My Display",
            Some("OpenRouter"),
            ProviderKind::CandleVllm
        )),
        "openrouter"
    );
    assert_eq!(
        reg.derive_id(&blank_conn("LM Studio", None, ProviderKind::CandleVllm)),
        "lm-studio"
    );
    // Blank vendor + blank display -> kind slug.
    assert_eq!(
        reg.derive_id(&blank_conn("   ", None, ProviderKind::Ollama)),
        "ollama"
    );
}

#[test]
fn upsert_appends_with_derived_id_then_edits_in_place() {
    let mut reg = ProviderRegistry::default();
    let stored = reg.upsert(blank_conn(
        "OpenRouter",
        Some("openrouter"),
        ProviderKind::CandleVllm,
    ));
    assert_eq!(stored.id, "openrouter");
    assert_eq!(reg.connections.len(), 3);
    // Editing the same id replaces in place (no new entry).
    let edited = reg.upsert(ProviderConnection {
        model: "new-model".to_string(),
        ..stored.clone()
    });
    assert_eq!(edited.model, "new-model");
    assert_eq!(reg.connections.len(), 3);
    assert_eq!(
        reg.connections
            .iter()
            .find(|c| c.id == "openrouter")
            .unwrap()
            .model,
        "new-model"
    );
}

#[test]
fn remove_rejects_last_and_reassigns_active() {
    let mut reg = ProviderRegistry::default();
    reg.set_active("ollama").unwrap();
    reg.remove("ollama").unwrap();
    assert_eq!(reg.connections.len(), 1);
    assert_eq!(reg.active, "candle-vllm");
    // Now only one remains -> reject.
    assert!(reg.remove("candle-vllm").is_err());
}

#[test]
fn remove_unknown_is_noop() {
    let mut reg = ProviderRegistry::default();
    reg.remove("does-not-exist").unwrap();
    assert_eq!(reg.connections.len(), 2);
}

#[test]
fn set_active_rejects_unknown() {
    let mut reg = ProviderRegistry::default();
    assert!(reg.set_active("nope").is_err());
    assert_eq!(reg.active, "candle-vllm");
}

#[test]
fn active_connection_is_none_when_pointer_dangles() {
    let reg = ProviderRegistry {
        active: "missing".to_string(),
        ..ProviderRegistry::default()
    };
    assert!(reg.active_connection().is_none());
}

#[test]
fn connection_resolved_base_url_falls_back_to_kind_default() {
    let conn = ProviderConnection {
        id: "ollama".into(),
        kind: ProviderKind::Ollama,
        display_name: "Ollama".into(),
        vendor: None,
        base_url: None,
        model: "llama3.2".into(),
        has_key: false,
        secret_missing: false,
        thinking: true,
        reasoning_effort: ReasoningEffort::default(),
        reasoning_visibility: ReasoningVisibility::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
    };
    assert_eq!(conn.resolved_base_url(), "http://localhost:11434");
    let overridden = ProviderConnection {
        base_url: Some("http://example:9000".into()),
        ..conn
    };
    assert_eq!(overridden.resolved_base_url(), "http://example:9000");
}

#[test]
fn registry_round_trips_through_json() {
    let reg = ProviderRegistry::default();
    let json = serde_json::to_string(&reg).unwrap();
    assert!(json.contains("\"active\""));
    assert!(json.contains("candle-vllm"));
    let back: ProviderRegistry = serde_json::from_str(&json).unwrap();
    assert_eq!(reg, back);
}

#[test]
fn num_ctx_absent_deserializes_to_none_and_round_trips() {
    // A pre-#651 connection (no `numCtx`) must load as `None` and re-serialize
    // without the key, so old registries keep the env→probe→default behavior.
    let conn = blank_conn("Ollama", None, ProviderKind::Ollama);
    assert_eq!(conn.num_ctx, None);
    let json = serde_json::to_string(&conn).unwrap();
    assert!(!json.contains("numCtx"), "None must omit the field: {json}");
    let back: ProviderConnection = serde_json::from_str(&json).unwrap();
    assert_eq!(back.num_ctx, None);

    // A set value round-trips through the camelCase wire key.
    let set = ProviderConnection {
        num_ctx: Some(8192),
        ..conn
    };
    let json = serde_json::to_string(&set).unwrap();
    assert!(json.contains("\"numCtx\":8192"), "{json}");
    let back: ProviderConnection = serde_json::from_str(&json).unwrap();
    assert_eq!(back.num_ctx, Some(8192));
}

#[test]
fn kind_deserializes_from_camel_case() {
    let k: ProviderKind = serde_json::from_str("\"ollama\"").unwrap();
    assert_eq!(k, ProviderKind::Ollama);
    let k: ProviderKind = serde_json::from_str("\"candleVllm\"").unwrap();
    assert_eq!(k, ProviderKind::CandleVllm);
}

#[test]
fn bedrock_kind_slug_and_base_url() {
    assert_eq!(ProviderKind::Bedrock.slug(), "bedrock");
    assert_eq!(
        ProviderKind::Bedrock.default_base_url(),
        "https://bedrock-runtime.us-east-1.amazonaws.com"
    );
    let k: ProviderKind = serde_json::from_str("\"bedrock\"").unwrap();
    assert_eq!(k, ProviderKind::Bedrock);
}

#[test]
fn openai_kind_slug_and_base_url() {
    assert_eq!(ProviderKind::OpenAi.slug(), "openai");
    assert_eq!(
        ProviderKind::OpenAi.default_base_url(),
        "https://api.openai.com/v1"
    );
}

#[test]
fn openai_kind_wire_tag_is_pinned_not_camel_case() {
    // The variant is pinned to "openai" via #[serde(rename)], NOT the
    // camelCase default "openAi". Round-trips and matches slug()/vendor.
    assert_eq!(
        serde_json::to_string(&ProviderKind::OpenAi).unwrap(),
        "\"openai\""
    );
    let k: ProviderKind = serde_json::from_str("\"openai\"").unwrap();
    assert_eq!(k, ProviderKind::OpenAi);
}

#[test]
fn bedrock_auth_serializes_camel_case() {
    for (variant, wire) in [
        (BedrockAuth::Auto, "\"auto\""),
        (BedrockAuth::Profile, "\"profile\""),
        (BedrockAuth::IamKeys, "\"iamKeys\""),
        (BedrockAuth::ApiKey, "\"apiKey\""),
    ] {
        assert_eq!(serde_json::to_string(&variant).unwrap(), wire);
        assert_eq!(serde_json::from_str::<BedrockAuth>(wire).unwrap(), variant);
    }
}

#[test]
fn resolve_auto_prefers_api_key_then_profile_then_iam_keys() {
    use BedrockAuth::*;
    // (has_api_key, has_profile, has_iam_keys) => expected winner
    assert_eq!(BedrockAuth::resolve_auto(true, true, true), ApiKey);
    assert_eq!(BedrockAuth::resolve_auto(false, true, true), Profile);
    assert_eq!(BedrockAuth::resolve_auto(false, false, true), IamKeys);
    assert_eq!(BedrockAuth::resolve_auto(true, false, false), ApiKey);
    assert_eq!(BedrockAuth::resolve_auto(false, true, false), Profile);
    // Nothing configured falls back to Profile so the probe surfaces the failure.
    assert_eq!(BedrockAuth::resolve_auto(false, false, false), Profile);
}

#[test]
fn secret_kind_slug_all_and_serde() {
    assert_eq!(SecretKind::ALL.len(), 3);
    assert_eq!(SecretKind::ApiKey.slug(), "apiKey");
    assert_eq!(SecretKind::SecretAccessKey.slug(), "secretAccessKey");
    assert_eq!(SecretKind::SessionToken.slug(), "sessionToken");
    for kind in SecretKind::ALL {
        let wire = serde_json::to_string(&kind).unwrap();
        assert_eq!(wire, format!("\"{}\"", kind.slug()));
        assert_eq!(serde_json::from_str::<SecretKind>(&wire).unwrap(), kind);
    }
}

#[test]
fn connection_skips_none_bedrock_fields_and_round_trips() {
    let conn = blank_conn("Local", None, ProviderKind::Ollama);
    let json = serde_json::to_string(&conn).unwrap();
    assert!(!json.contains("region"), "None region is skipped");
    assert!(!json.contains("authMode"), "None auth_mode is skipped");
    assert!(!json.contains("awsProfile"));
    assert!(!json.contains("accessKeyId"));
    let back: ProviderConnection = serde_json::from_str(&json).unwrap();
    assert_eq!(conn, back);
}

#[test]
fn model_supports_vision_covers_known_families() {
    // Modern Claude on Bedrock: 3.x and 4.x, plus the named Mythos/Fable.
    for m in [
        "us.anthropic.claude-opus-4-8",
        "us.anthropic.claude-opus-4-5",
        "us.anthropic.claude-sonnet-4-6",
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
        "us.anthropic.claude-3-opus-20240229-v1:0",
        "claude-mythos-5",
        "claude-fable-5",
    ] {
        assert!(
            model_supports_vision(ProviderKind::Bedrock, m),
            "Bedrock vision: {m}"
        );
    }
    // OpenAI vision-capable.
    for m in [
        "gpt-4o",
        "gpt-4o-mini",
        "gpt-4.1",
        "gpt-4-turbo",
        "gpt-5",
        "o1",
        "o3-mini",
    ] {
        assert!(
            model_supports_vision(ProviderKind::OpenAi, m),
            "OpenAI vision: {m}"
        );
    }
    // Ollama vision tags.
    for m in ["llava:7b", "llama3.2-vision", "moondream", "qwen2-vl:7b"] {
        assert!(
            model_supports_vision(ProviderKind::Ollama, m),
            "Ollama vision: {m}"
        );
    }
    // SiliconFlow VL/4V suffixes.
    for m in ["Qwen/Qwen2-VL-7B-Instruct", "zai-org/GLM-4V-9B"] {
        assert!(
            model_supports_vision(ProviderKind::SiliconFlow, m),
            "SiliconFlow vision: {m}"
        );
    }
    // Negative coverage: text-only stays text-only.
    for (k, m) in [
        (ProviderKind::Ollama, "llama3.2"),
        (ProviderKind::OpenAi, "gpt-3.5-turbo"),
        (ProviderKind::Bedrock, "meta.llama3-70b-instruct-v1:0"),
        (ProviderKind::SiliconFlow, "deepseek-ai/DeepSeek-V3"),
        (ProviderKind::CandleVllm, "anything"),
    ] {
        assert!(
            !model_supports_vision(k, m),
            "expected text-only: {k:?} {m}"
        );
    }
}

#[test]
fn model_supports_documents_is_universal() {
    // As of the #338 follow-up, every provider kind supports document
    // attachments: Bedrock via native `DocumentBlock`, OpenAI-compatible /
    // Ollama via the client-side text-extraction fallback. The UI's document
    // attachment gate (`ResolvedModel.supports_documents`) follows this, so a
    // non-Bedrock session can still stage a document for extraction.
    for k in [
        ProviderKind::Bedrock,
        ProviderKind::OpenAi,
        ProviderKind::SiliconFlow,
        ProviderKind::Ollama,
        ProviderKind::CandleVllm,
    ] {
        for m in [
            "anything",
            "us.anthropic.claude-opus-4-8",
            "gpt-4o",
            "llama3.2",
        ] {
            assert!(
                model_supports_documents(k, m),
                "{k:?} + {m} should support documents"
            );
        }
    }
}

#[test]
fn connection_with_bedrock_fields_round_trips() {
    let conn = ProviderConnection {
        region: Some("us-west-2".into()),
        auth_mode: Some(BedrockAuth::IamKeys),
        access_key_id: Some("AKIAEXAMPLE".into()),
        compaction_model: None,
        compaction_budget: None,
        ..blank_conn("Bedrock", Some("aws"), ProviderKind::Bedrock)
    };
    let json = serde_json::to_string(&conn).unwrap();
    assert!(json.contains("\"region\":\"us-west-2\""));
    assert!(json.contains("\"authMode\":\"iamKeys\""));
    assert!(json.contains("\"accessKeyId\":\"AKIAEXAMPLE\""));
    let back: ProviderConnection = serde_json::from_str(&json).unwrap();
    assert_eq!(conn, back);
}
