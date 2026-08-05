use super::{channel_map_path, load_provider, resolve_phenotype_in, session_db_path};
use ff_core::{
    ProviderConfig, ProviderConnection, ProviderKind, ProviderRegistry, ReasoningEffort, SecretKind,
};
use ff_llm::{ChatMessage, ChatRequest, Provider};
use ff_skills::DEFAULT_PHENOTYPE;
use std::fs;

#[test]
fn default_phenotype_resolves_without_any_files() {
    let tmp = tempfile::tempdir().unwrap();
    let p = resolve_phenotype_in(DEFAULT_PHENOTYPE, tmp.path()).unwrap();
    assert_eq!(p.name, "default");
    assert!(p.skills.is_empty());
    assert!(p.model.is_none());
    assert!(p.persona.is_none());
}

#[test]
fn resolves_named_phenotype_from_toml_by_stem() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(
        tmp.path().join("rust.toml"),
        "skills = [\"cargo-check\", \"clippy\"]\n\
         model = \"qwen3-coder\"\n\
         persona = \"You are a Rust expert.\"\n",
    )
    .unwrap();

    let p = resolve_phenotype_in("rust", tmp.path()).unwrap();
    assert_eq!(p.name, "rust");
    assert_eq!(p.skills, vec!["cargo-check", "clippy"]);
    assert_eq!(p.model.as_deref(), Some("qwen3-coder"));
    assert_eq!(p.persona.as_deref(), Some("You are a Rust expert."));
}

#[test]
fn unknown_name_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(resolve_phenotype_in("nope", tmp.path()).is_none());
}

#[test]
fn api_key_from_env_reads_a_set_key() {
    let var = "FF_TEST_OPENAI_KEY_SET";
    std::env::set_var(var, "sk-abc123");
    assert_eq!(super::api_key_from_env(var), Some("sk-abc123".to_string()));
    std::env::remove_var(var);
}

#[test]
fn api_key_from_env_treats_empty_as_keyless() {
    let var = "FF_TEST_OPENAI_KEY_EMPTY";
    std::env::set_var(var, "");
    assert_eq!(super::api_key_from_env(var), None);
    std::env::remove_var(var);
}

#[test]
fn api_key_from_env_unset_is_none() {
    assert_eq!(
        super::api_key_from_env("FF_TEST_OPENAI_KEY_NEVER_SET"),
        None
    );
}

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn siliconflow_body(thinking: bool, effort: ReasoningEffort) -> serde_json::Value {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string("data: [DONE]\n\n"))
        .mount(&server)
        .await;

    let config = ProviderConfig {
        kind: ProviderKind::SiliconFlow,
        base_url: Some(server.uri()),
        model: "zai-org/GLM-5.2".into(),
        reasoning_effort: effort,
        ..Default::default()
    };
    let provider = super::build_provider(&config);
    let req = ChatRequest {
        model: config.model.clone(),
        messages: vec![ChatMessage::text("user", "hi")],
        tools: Vec::new(),
        thinking,
        max_tokens: None,
        cache_messages: false,
    };
    let _ = provider.chat_stream(req).await.expect("send succeeds");
    let reqs = server.received_requests().await.expect("requests recorded");
    serde_json::from_slice(&reqs[0].body).expect("body is json")
}

#[tokio::test]
async fn siliconflow_cli_provider_emits_thinking_budget() {
    let body = siliconflow_body(true, ReasoningEffort::Medium).await;
    assert_eq!(body["thinking_budget"], 4096);
    assert!(body.get("enable_thinking").is_none());
}

#[tokio::test]
async fn siliconflow_cli_provider_effort_dial_is_honored() {
    let body = siliconflow_body(true, ReasoningEffort::Low).await;
    assert_eq!(body["thinking_budget"], 1024);
}

#[tokio::test]
async fn siliconflow_cli_provider_thinking_off_disables_reasoning() {
    let body = siliconflow_body(false, ReasoningEffort::Medium).await;
    assert_eq!(body["enable_thinking"], false);
    assert!(body.get("thinking_budget").is_none());
}

#[test]
fn bedrock_cli_provider_forwards_reasoning_effort() {
    let config = ProviderConfig {
        kind: ProviderKind::Bedrock,
        model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
        reasoning_effort: ReasoningEffort::High,
        ..Default::default()
    };
    let provider = super::build_bedrock_provider(&config, true);
    assert_eq!(provider.reasoning_effort(), ReasoningEffort::High);
    assert!(
        provider.supports_documents(),
        "documents flag forwarded to the Bedrock provider"
    );
}

#[test]
fn bedrock_cli_provider_default_effort_is_medium() {
    let config = ProviderConfig {
        kind: ProviderKind::Bedrock,
        model: "anthropic.claude-3-5-sonnet-20241022-v2:0".into(),
        ..Default::default()
    };
    let provider = super::build_bedrock_provider(&config, true);
    assert_eq!(provider.reasoning_effort(), ReasoningEffort::Medium);
}

/// The channel→session map must live beside the session DB (#1060).
///
/// `serve` shares its store with the desktop app so a Slack conversation shows up
/// in the UI; if the map resolved to a different root, the mapping and the
/// sessions it names would drift apart and every restart would strand the
/// channel's history. Deriving both from `config_dir` is what prevents that, and
/// this asserts it rather than trusting the two call sites to stay in step.
#[test]
fn channel_map_sits_beside_the_session_db() {
    let _env = crate::test_support::TestEnv::new();

    let map = channel_map_path().expect("override provides a config dir");
    let db = session_db_path().expect("override provides a config dir");

    assert_eq!(
        map.parent(),
        db.parent(),
        "channel map and session DB must share a directory"
    );
    assert_eq!(map.file_name().unwrap(), "channel-map.json");
}

/// #1060: a missing `provider.json` must not resolve silently.
///
/// The default is local candle-vllm on port 8000, so a silent fallback made every
/// `serve` turn fail with `error sending request for url
/// (http://localhost:8000/v1/chat/completions)` — an error naming a port the user
/// never chose, with nothing connecting it to an absent config file.
#[test]
fn missing_provider_config_falls_back_to_the_default() {
    let env = crate::test_support::TestEnv::new();
    assert!(
        !env.legacy_path().exists(),
        "precondition: no provider.json in the test env"
    );

    let config = super::load_provider_config();

    assert_eq!(config.kind, ProviderKind::default());
    assert_eq!(
        super::provider_config_path().unwrap(),
        env.legacy_path(),
        "must read the overridden dir, not the real ~/.config"
    );
}

/// A malformed file falls back rather than panicking — and is distinguished from
/// the missing-file case, since the remedies differ (fix the file vs create it).
#[test]
fn malformed_provider_config_falls_back_to_the_default() {
    let env = crate::test_support::TestEnv::new();
    fs::write(env.legacy_path(), "{ not json").unwrap();

    let config = super::load_provider_config();

    assert_eq!(config.kind, ProviderKind::default());
}

/// The complement, and the one that stops the warnings becoming noise: a valid
/// config is honored and must NOT hit any fallback path.
#[test]
fn valid_provider_config_is_honored() {
    let env = crate::test_support::TestEnv::new();
    fs::write(
        env.legacy_path(),
        r#"{"kind":"ollama","model":"qwen3.6:35b-a3b","hasKey":false}"#,
    )
    .unwrap();

    let config = super::load_provider_config();

    assert_eq!(config.kind, ProviderKind::Ollama);
    assert_eq!(config.model, "qwen3.6:35b-a3b");
    assert_ne!(
        config.kind,
        ProviderKind::default(),
        "guards the test itself: a default-kind fixture could not detect a fallback"
    );
}

/// The fallback description must name the port that appears in the resulting
/// request error, which is the only string tying symptom to cause.
#[test]
fn default_provider_desc_names_the_url_from_the_error() {
    let desc = super::default_provider_desc();
    assert!(desc.contains("localhost:8000"), "got {desc}");
    assert!(desc.contains("candle-vllm"), "got {desc}");
}

// ---------------------------------------------------------------------------
// Registry-active-connection tests (#1199)
// ---------------------------------------------------------------------------

/// Regression: a provider selected in the GUI (active in provider-registry.json)
/// must take effect in `load_provider()`. Write a registry with `ollama` active
/// and a *differing* legacy `provider.json`, then verify the (registry) model
/// wins — proving registry beats legacy, which is the entire point of #1199.
#[test]
fn registry_active_connection_wins() {
    let env = crate::test_support::TestEnv::new();
    let conn = ProviderConnection {
        id: "ollama".into(),
        kind: ProviderKind::Ollama,
        display_name: "Ollama".into(),
        vendor: None,
        base_url: None,
        model: "qwen3.6:35b-a3b".into(),
        has_key: false,
        secret_missing: false,
        thinking: true,
        reasoning_effort: ReasoningEffort::Medium,
        reasoning_visibility: Default::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
        near_budget: None,
    };
    let registry = ProviderRegistry {
        active: "ollama".into(),
        connections: vec![conn],
        schema_version: 0,
    };
    env.write_registry(&registry);
    // A legacy provider.json is present *and* differs. If the registry
    // is preferred (the fix for #1199) the model below is qwen3.6:35b-a3b;
    // if legacy is preferred the model is LEGACY-MUST-NOT-WIN. Either way
    // the assertion fails on the wrong answer, not a silent fallback.
    fs::write(
        env.legacy_path(),
        r#"{"kind":"ollama","model":"LEGACY-MUST-NOT-WIN","hasKey":false}"#,
    )
    .unwrap();

    let (_provider, model) = load_provider();

    assert_eq!(model, "qwen3.6:35b-a3b");
}

/// A dangling active pointer (registry exists but no connection matches)
/// falls through to the legacy `provider.json` when present, not to the
/// default — preserving the pre-registry install behaviour.
#[test]
fn registry_dangling_active_falls_back_to_legacy() {
    let env = crate::test_support::TestEnv::new();
    // Registry exists but active points at a connection not in the list.
    let registry = ProviderRegistry {
        active: "nonexistent".into(),
        connections: vec![],
        schema_version: 0,
    };
    env.write_registry(&registry);
    // Legacy file exists with a different provider.
    fs::write(
        env.legacy_path(),
        r#"{"kind":"ollama","model":"qwen3.6:35b-a3b","hasKey":false}"#,
    )
    .unwrap();

    let (_provider, model) = load_provider();

    // Should fall through to legacy, not default.
    assert_eq!(model, "qwen3.6:35b-a3b");
}

/// When neither registry nor legacy file exists, `load_provider()` falls back
/// to the built-in default (candle-vllm) — the same behaviour as before the
/// registry adoption, preserving fresh-install experience.
#[test]
fn load_provider_falls_back_to_default_when_nothing_present() {
    let env = crate::test_support::TestEnv::new();
    assert!(!env.registry_path().exists(), "no registry file");
    assert!(!env.legacy_path().exists(), "no legacy file");

    let (_provider, model) = load_provider();

    assert_eq!(model, ProviderConfig::default().model);
}

// ---------------------------------------------------------------------------
// api_key_from_env_or_keyring tests (#1199)
// ---------------------------------------------------------------------------

#[test]
fn api_key_from_env_or_keyring_env_var_wins() {
    let var = "FF_TEST_KEY_OVERRIDE";
    std::env::set_var(var, "sk-from-env");
    let conn = ProviderConnection {
        id: "test-conn".into(),
        kind: ProviderKind::OpenAi,
        ..connection_defaults()
    };
    // Set a key in the keyring — should be shadowed by the env var.
    crate::secrets::set("test-conn", SecretKind::ApiKey, "sk-from-keyring").expect("set keyring");

    let result = super::api_key_from_env_or_keyring(&conn, var);

    assert_eq!(result.as_deref(), Some("sk-from-env"));
    std::env::remove_var(var);
}

#[test]
fn api_key_from_env_or_keyring_falls_back_to_keyring() {
    let conn = ProviderConnection {
        id: "test-conn-fallback".into(),
        kind: ProviderKind::OpenAi,
        ..connection_defaults()
    };
    crate::secrets::set("test-conn-fallback", SecretKind::ApiKey, "sk-from-keyring")
        .expect("set keyring");

    let result = super::api_key_from_env_or_keyring(&conn, "FF_TEST_KEY_NOT_SET");

    assert_eq!(result.as_deref(), Some("sk-from-keyring"));
}

#[test]
fn api_key_from_env_or_keyring_returns_none_when_both_absent() {
    let conn = ProviderConnection {
        id: "test-conn-none".into(),
        kind: ProviderKind::OpenAi,
        ..connection_defaults()
    };

    let result = super::api_key_from_env_or_keyring(&conn, "FF_TEST_KEY_NEVER_SET");

    assert!(result.is_none());
}

fn connection_defaults() -> ProviderConnection {
    ProviderConnection {
        id: String::new(),
        kind: ProviderKind::CandleVllm,
        display_name: String::new(),
        vendor: None,
        base_url: None,
        model: String::new(),
        has_key: false,
        secret_missing: false,
        thinking: false,
        reasoning_effort: ReasoningEffort::default(),
        reasoning_visibility: Default::default(),
        warmup_enabled: true,
        num_ctx: None,
        region: None,
        auth_mode: None,
        aws_profile: None,
        access_key_id: None,
        compaction_model: None,
        compaction_budget: None,
        near_budget: None,
    }
}
